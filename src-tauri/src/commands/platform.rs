
use slugtale_lib::WeekStart;

pub(crate) trait PlatformAdapter {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
    fn open_microphone_settings() -> Result<(), String>;
    fn open_text_insertion_settings() -> Result<(), String>;
    fn locale_week_start() -> WeekStart;
}

#[cfg(target_os = "macos")]
type Adapter = slugtale_lib::MacosPlatform;

#[cfg(target_os = "windows")]
type Adapter = slugtale_lib::WindowsPlatform;

#[cfg(target_os = "linux")]
type Adapter = slugtale_lib::LinuxPlatform;

/// The answer for a build with no platform adapter behind it: no permission is
/// granted, no shortcut exists, and the ISO week is assumed.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
#[derive(Default)]
struct UnsupportedPlatform;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
type Adapter = UnsupportedPlatform;

#[cfg(target_os = "macos")]
impl PlatformAdapter for slugtale_lib::MacosPlatform {
    fn microphone_granted(&self) -> bool {
        slugtale_lib::MacosPlatform::microphone_granted(self)
    }

    fn insertion_granted(&self) -> bool {
        slugtale_lib::MacosPlatform::insertion_granted(self)
    }

    fn open_microphone_settings() -> Result<(), String> {
        slugtale_lib::run_microphone_permission_setup(&slugtale_lib::MacosMicrophonePermissionSetup)
    }

    fn open_text_insertion_settings() -> Result<(), String> {
        slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::MacosTextInsertionPermissionSetup,
        )
        .map(|_| ())
    }

    fn locale_week_start() -> WeekStart {
        slugtale_lib::locale_week_start()
    }
}

#[cfg(target_os = "windows")]
impl PlatformAdapter for slugtale_lib::WindowsPlatform {
    fn microphone_granted(&self) -> bool {
        slugtale_lib::WindowsPlatform::microphone_granted(self)
    }

    fn insertion_granted(&self) -> bool {
        slugtale_lib::WindowsPlatform::insertion_granted(self)
    }

    fn open_microphone_settings() -> Result<(), String> {
        slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::WindowsMicrophonePermissionSetup,
        )
    }

    fn open_text_insertion_settings() -> Result<(), String> {
        slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::WindowsTextInsertionPermissionSetup,
        )
        .map(|_| ())
    }

    fn locale_week_start() -> WeekStart {
        slugtale_lib::locale_week_start()
    }
}

#[cfg(target_os = "linux")]
impl PlatformAdapter for slugtale_lib::LinuxPlatform {
    fn microphone_granted(&self) -> bool {
        slugtale_lib::LinuxPlatform::microphone_granted(self)
    }

    fn insertion_granted(&self) -> bool {
        slugtale_lib::LinuxPlatform::insertion_granted(self)
    }

    fn open_microphone_settings() -> Result<(), String> {
        slugtale_lib::run_microphone_permission_setup(&slugtale_lib::LinuxMicrophonePermissionSetup)
    }

    fn open_text_insertion_settings() -> Result<(), String> {
        slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::LinuxTextInsertionPermissionSetup,
        )
        .map(|_| ())
    }

    fn locale_week_start() -> WeekStart {
        slugtale_lib::locale_week_start()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl PlatformAdapter for UnsupportedPlatform {
    fn microphone_granted(&self) -> bool {
        false
    }

    fn insertion_granted(&self) -> bool {
        false
    }

    fn open_microphone_settings() -> Result<(), String> {
        Err("microphone settings shortcut is not implemented for this platform".to_string())
    }

    fn open_text_insertion_settings() -> Result<(), String> {
        Err("text insertion settings shortcut is not implemented for this platform".to_string())
    }

    fn locale_week_start() -> WeekStart {
        WeekStart::default()
    }
}

#[derive(Default)]
pub(crate) struct CurrentPlatform(Adapter);

impl CurrentPlatform {
    pub(crate) fn new() -> Self {
        Self(Adapter::default())
    }

    pub(crate) fn microphone_granted(&self) -> bool {
        PlatformAdapter::microphone_granted(&self.0)
    }

    pub(crate) fn insertion_granted(&self) -> bool {
        PlatformAdapter::insertion_granted(&self.0)
    }
}

#[tauri::command]
pub(crate) fn open_microphone_settings() -> Result<(), String> {
    <Adapter as PlatformAdapter>::open_microphone_settings()
}

#[tauri::command]
pub(crate) fn open_text_insertion_settings() -> Result<(), String> {
    <Adapter as PlatformAdapter>::open_text_insertion_settings()
}

pub(crate) fn locale_week_start() -> WeekStart {
    <Adapter as PlatformAdapter>::locale_week_start()
}
