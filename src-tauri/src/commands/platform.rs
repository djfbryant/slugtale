//! OS platform adapters behind the binary's commands: the microphone and text
//! insertion permission probes the readiness report reads, the permission-setup
//! shortcuts Settings offers, and the locale's week start.

#[derive(Default)]
pub(crate) struct CurrentPlatform;

impl CurrentPlatform {
    pub(crate) fn new() -> Self {
        Self
    }

    #[cfg(target_os = "macos")]
    fn macos_platform(&self) -> slugtale_lib::MacosPlatform {
        slugtale_lib::MacosPlatform::new()
    }

    #[cfg(target_os = "windows")]
    fn windows_platform(&self) -> slugtale_lib::WindowsPlatform {
        slugtale_lib::WindowsPlatform::new()
    }

    #[cfg(target_os = "linux")]
    fn linux_platform(&self) -> slugtale_lib::LinuxPlatform {
        slugtale_lib::LinuxPlatform::new()
    }
}

impl CurrentPlatform {
    pub(crate) fn microphone_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().microphone_granted();
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_platform().microphone_granted();
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_platform().microphone_granted();
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }

    pub(crate) fn insertion_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().insertion_granted();
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_platform().insertion_granted();
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_platform().insertion_granted();
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }
}

#[tauri::command]
pub(crate) fn open_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::MacosMicrophonePermissionSetup,
        );
    }

    #[cfg(target_os = "windows")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::WindowsMicrophonePermissionSetup,
        );
    }

    #[cfg(target_os = "linux")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::LinuxMicrophonePermissionSetup,
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("microphone settings shortcut is not implemented for this platform".to_string())
    }
}

#[tauri::command]
pub(crate) fn open_text_insertion_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::MacosTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(target_os = "windows")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::WindowsTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(target_os = "linux")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::LinuxTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("text insertion settings shortcut is not implemented for this platform".to_string())
    }
}

/// Which week the Usage pane means by "this week", asked of the OS rather than
/// assumed (ADR-0021: locale is platform behaviour).
pub(crate) fn locale_week_start(_app: &tauri::AppHandle) -> slugtale_lib::WeekStart {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        slugtale_lib::locale_week_start()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        slugtale_lib::WeekStart::default()
    }
}
