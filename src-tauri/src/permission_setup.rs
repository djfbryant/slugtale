pub trait MicrophonePermissionSetup {
    fn request_microphone_access(&self) -> Result<(), String>;
    fn open_microphone_settings(&self) -> Result<(), String>;
}

pub fn run_microphone_permission_setup(
    system: &dyn MicrophonePermissionSetup,
) -> Result<(), String> {
    let request_result = system.request_microphone_access();
    let open_result = system.open_microphone_settings();

    open_result?;
    request_result
}

pub trait TextInsertionPermissionSetup {
    fn request_text_insertion_access(&self) -> Result<bool, String>;
    fn open_text_insertion_settings(&self) -> Result<(), String>;
}

pub fn run_text_insertion_permission_setup(
    system: &dyn TextInsertionPermissionSetup,
) -> Result<bool, String> {
    let trusted = system.request_text_insertion_access()?;
    if !trusted {
        system.open_text_insertion_settings()?;
    }
    Ok(trusted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_permission_setup_requests_access_before_opening_settings() {
        let system = FakeMicrophonePermissionSetup::default();

        run_microphone_permission_setup(&system).unwrap();

        assert_eq!(
            system.events.borrow().as_slice(),
            &["request_microphone_access", "open_microphone_settings"]
        );
    }
    #[test]
    fn text_insertion_permission_setup_opens_settings_only_when_not_trusted() {
        let system = FakeTextInsertionPermissionSetup::untrusted();

        let trusted = run_text_insertion_permission_setup(&system).unwrap();

        assert!(!trusted);
        assert_eq!(
            system.events.borrow().as_slice(),
            &[
                "request_text_insertion_access",
                "open_text_insertion_settings"
            ]
        );
    }
    #[test]
    fn text_insertion_permission_setup_does_not_reopen_settings_when_trusted() {
        let system = FakeTextInsertionPermissionSetup::trusted();

        let trusted = run_text_insertion_permission_setup(&system).unwrap();

        assert!(trusted);
        assert_eq!(
            system.events.borrow().as_slice(),
            &["request_text_insertion_access"]
        );
    }

    #[derive(Default)]
    struct FakeMicrophonePermissionSetup {
        events: std::cell::RefCell<Vec<&'static str>>,
    }

    impl MicrophonePermissionSetup for FakeMicrophonePermissionSetup {
        fn request_microphone_access(&self) -> Result<(), String> {
            self.events.borrow_mut().push("request_microphone_access");
            Ok(())
        }

        fn open_microphone_settings(&self) -> Result<(), String> {
            self.events.borrow_mut().push("open_microphone_settings");
            Ok(())
        }
    }

    struct FakeTextInsertionPermissionSetup {
        events: std::cell::RefCell<Vec<&'static str>>,
        trusted: bool,
    }

    impl FakeTextInsertionPermissionSetup {
        fn trusted() -> Self {
            Self {
                events: std::cell::RefCell::new(Vec::new()),
                trusted: true,
            }
        }

        fn untrusted() -> Self {
            Self {
                events: std::cell::RefCell::new(Vec::new()),
                trusted: false,
            }
        }
    }

    impl TextInsertionPermissionSetup for FakeTextInsertionPermissionSetup {
        fn request_text_insertion_access(&self) -> Result<bool, String> {
            self.events
                .borrow_mut()
                .push("request_text_insertion_access");
            Ok(self.trusted)
        }

        fn open_text_insertion_settings(&self) -> Result<(), String> {
            self.events
                .borrow_mut()
                .push("open_text_insertion_settings");
            Ok(())
        }
    }
}
