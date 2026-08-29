//! Hotkey registration and the global-key worker: which shortcut starts and
//! stops dictation, when bare Escape is global, and the single worker thread
//! that turns key transitions into lifecycle events.

use std::sync::Mutex;

use tauri::Manager;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use super::app_paths::load_current_settings;
use super::{begin_dictation, dictation_host, typing_challenge_is_open};

const DICTATION_ESCAPE_KEY: &str = "Escape";

#[derive(Default)]
pub(super) struct HotkeyRegistrationState(pub(super) Mutex<HotkeyRegistration>);

#[derive(Default)]
pub(super) struct HotkeyRegistration {
    pub(super) current_hotkey: Option<String>,
    pub(super) control: slugtale_lib::DictationControl,
    pub(super) key_commands: Option<std::sync::mpsc::Sender<GlobalKeyCommand>>,
}

#[derive(Clone, Copy)]
pub(super) enum GlobalKeyCommand {
    Input(slugtale_lib::DictationKey, slugtale_lib::HotkeyInput),
    SyncEscape(slugtale_lib::EscapeCommand),
}

pub(super) fn setup_configured_hotkey(
    app: &mut tauri::App,
) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_current_settings(app.handle());

    let mut builder =
        tauri_plugin_global_shortcut::Builder::new().with_handler(move |app, shortcut, event| {
            let input = match event.state {
                tauri_plugin_global_shortcut::ShortcutState::Pressed => {
                    slugtale_lib::HotkeyInput::Pressed
                }
                tauri_plugin_global_shortcut::ShortcutState::Released => {
                    slugtale_lib::HotkeyInput::Released
                }
            };

            let state = app.state::<HotkeyRegistrationState>();
            let registration = state.0.lock();
            match registration {
                Ok(registration) => {
                    if let Some(commands) = registration.key_commands.as_ref() {
                        let key = if shortcut.key == tauri_plugin_global_shortcut::Code::Escape
                            && shortcut.mods.is_empty()
                        {
                            slugtale_lib::DictationKey::Escape
                        } else {
                            slugtale_lib::DictationKey::Hotkey
                        };
                        let _ = commands.send(GlobalKeyCommand::Input(key, input));
                    }
                }
                Err(_) => eprintln!("hotkey dictation adapter mutex poisoned"),
            }
        });

    if let Some(hotkey) = settings.hotkey.as_deref() {
        builder = builder.with_shortcut(hotkey)?;
    }

    app.handle().plugin(builder.build())?;
    set_hotkey_registration_state(app.handle(), &settings)?;
    start_global_key_worker(app.handle())?;
    Ok(())
}

pub(super) fn request_escape_registration(
    registration: &HotkeyRegistration,
    should_register: bool,
) -> Result<(), String> {
    let commands = registration
        .key_commands
        .as_ref()
        .ok_or_else(|| "global key worker has not started".to_string())?;
    commands
        .send(GlobalKeyCommand::SyncEscape(escape_command(
            should_register,
        )))
        .map_err(|_| "global key worker is unavailable".to_string())
}

/// A requested registration state expressed through the arbiter's vocabulary:
/// the caller says what it wants true/false to mean (armed or matching the
/// lifecycle), and the arbiter decides whether the OS must change.
fn escape_command(should_register: bool) -> slugtale_lib::EscapeCommand {
    if should_register {
        slugtale_lib::EscapeCommand::Arm
    } else {
        slugtale_lib::EscapeCommand::Disarm
    }
}

/// Bare Escape must only be global while recording; otherwise Slugtale would
/// steal Escape from the user's current application. A dedicated worker first
/// registers Escape and only then starts recording, so there is no active but
/// uncancellable window. It also keeps registration outside the plugin callback,
/// which holds the plugin's key map while invoking us.
fn start_global_key_worker(app: &tauri::AppHandle) -> Result<(), String> {
    let (commands, events) = std::sync::mpsc::channel::<GlobalKeyCommand>();
    {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration.key_commands = Some(commands);
    }

    let app = app.clone();
    std::thread::Builder::new()
        .name("dictation-global-keys".to_string())
        .spawn(move || {
            // The one owner of bare Escape's armed state on this thread.
            let mut escape_arbiter = slugtale_lib::EscapeArbiter::new();
            for event in events {
                match event {
                    GlobalKeyCommand::SyncEscape(command) => {
                        if let Err(error) =
                            sync_escape_registration(&app, &mut escape_arbiter, command)
                        {
                            eprintln!("could not update global Escape key: {error}");
                        }
                    }
                    GlobalKeyCommand::Input(key, input) => {
                        // The Typing Challenge guard also lives inside
                        // begin_dictation; this early check keeps the release of
                        // a swallowed key from reaching the lifecycle at all.
                        if typing_challenge_is_open(&app) {
                            continue;
                        }

                        let is_dictating = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .map(|registration| registration.control.is_dictating())
                            .unwrap_or(false);

                        let pressed_start = matches!(
                            (key, input),
                            (
                                slugtale_lib::DictationKey::Hotkey,
                                slugtale_lib::HotkeyInput::Pressed
                            )
                        );

                        // A start goes through the shared readiness-gated begin
                        // sequence, identical to the Voice Activation path.
                        // One snapshot for this press: the readiness gate and
                        // the Start path share the same Settings value and
                        // permission probes (slugtale-g1o.6).
                        if pressed_start && !is_dictating {
                            let mut set_escape = |should_register: bool| {
                                sync_escape_registration(
                                    &app,
                                    &mut escape_arbiter,
                                    escape_command(should_register),
                                )
                            };
                            if let Err(error) = begin_dictation(
                                &app,
                                slugtale_lib::DictationInput::Hotkey,
                                &mut set_escape,
                            ) {
                                eprintln!("dictation did not start: {error}");
                            }
                            continue;
                        }

                        let transition = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .and_then(|mut registration| {
                                let event = match (key, input) {
                                    (slugtale_lib::DictationKey::Hotkey, input) => {
                                        registration.control.on_hotkey(input)
                                    }
                                    (
                                        slugtale_lib::DictationKey::Escape,
                                        slugtale_lib::HotkeyInput::Pressed,
                                    ) => registration.control.cancel(),
                                    (
                                        slugtale_lib::DictationKey::Escape,
                                        slugtale_lib::HotkeyInput::Released,
                                    ) => None,
                                };
                                Some((event, registration.control.is_dictating()))
                            });
                        if let Some((event, should_register)) = transition {
                            // The shared registration mutex is no longer held:
                            // recording, transcription, and window work may block
                            // without preventing the main-thread shortcut handler
                            // from forwarding the next key transition (slugtale-pil).
                            if let Some(event) = event {
                                if let Err(error) =
                                    dictation_host(&app).handle_dictation_event(event)
                                {
                                    eprintln!("dictation event failed: {error}");
                                }
                            }
                            if let Err(error) = sync_escape_registration(
                                &app,
                                &mut escape_arbiter,
                                slugtale_lib::EscapeCommand::MatchDictation(should_register),
                            ) {
                                eprintln!("could not update global Escape key: {error}");
                            }
                        }
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(())
}

fn sync_escape_registration(
    app: &tauri::AppHandle,
    arbiter: &mut slugtale_lib::EscapeArbiter,
    command: slugtale_lib::EscapeCommand,
) -> Result<(), String> {
    let Some(should_register) = arbiter.resolve(command) else {
        return Ok(());
    };

    if should_register {
        app.global_shortcut()
            .register(DICTATION_ESCAPE_KEY)
            .map_err(|error| error.to_string())?;
    } else {
        app.global_shortcut()
            .unregister(DICTATION_ESCAPE_KEY)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn set_hotkey_registration_state(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    let state = app.state::<HotkeyRegistrationState>();
    let mut registration = state
        .0
        .lock()
        .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
    registration.current_hotkey = settings.hotkey.clone();
    // The lifecycle belongs to dictation, not to the optional hotkey. Voice
    // Activation and Dictation Bar controls use it even with no hotkey set.
    registration.control = slugtale_lib::DictationControl::new(settings.activation_mode);
    Ok(())
}

pub(super) fn update_registered_hotkey(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    let previous = {
        let state = app.state::<HotkeyRegistrationState>();
        let registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration.current_hotkey.clone()
    };
    let next = settings.hotkey.clone();

    if previous != next {
        if let Some(hotkey) = next.as_deref() {
            app.global_shortcut()
                .register(hotkey)
                .map_err(|error| error.to_string())?;
        }

        if let Some(hotkey) = previous.as_deref() {
            if let Err(error) = app.global_shortcut().unregister(hotkey) {
                if let Some(new_hotkey) = next.as_deref() {
                    let _ = app.global_shortcut().unregister(new_hotkey);
                }
                return Err(error.to_string());
            }
        }
    }

    set_hotkey_registration_state(app, settings)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub(super) fn update_registered_hotkey(
    _app: &tauri::AppHandle,
    _settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    Ok(())
}
