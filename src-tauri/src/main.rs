#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use tauri::Manager;

mod commands;
mod dictation_bar_window;
mod hotkey_registration;
mod voice_activation;

use hotkey_registration::{setup_configured_hotkey, HotkeyRegistrationState};
use slugtale_lib::{DictationHost, WindowLabel};

use slugtale_lib::AppFiles;

use slugtale_lib::TypingChallengeOpen;

// Every command the Settings and Dictation Bar windows can reach, imported by
// name so the one `generate_handler!` list below reads as the app's whole wire
// surface. Each adapter lives in the `commands` module for its domain.
use commands::dictation::{dictation_bar_displays, dictation_bar_pointer_over, dictation_event};
use commands::engines::{
    delete_local_model, download_local_model, get_local_model_status, install_engine_assets,
    remove_engine_assets, reveal_model_location, set_transcription_engines, transcription_engines,
};
use commands::platform::{open_microphone_settings, open_text_insertion_settings};
use commands::settings::{
    get_settings, get_settings_readiness, save_dictation_bar_settings, save_hotkey_settings,
    save_launch_at_login, save_microphone_settings, save_transcript_cleanup_settings,
    save_transcription_settings, save_voice_activation_settings, voice_activation_supported,
};
use commands::updates::{check_for_app_update, open_app_update_release};
use commands::usage::{
    close_typing_challenge, get_typing_challenge, get_usage_summary, open_typing_challenge,
    redo_typing_challenges, set_typing_estimate, set_usage_storing, submit_typing_challenge,
};

fn main() {
    let reauthorize_permissions =
        slugtale_lib::permission_reauthorization_requested(std::env::args());
    let app = tauri::Builder::default()
        .manage(slugtale_lib::TranscriptionEngineCatalogue::default())
        .manage(HotkeyRegistrationState::default())
        .manage(TypingChallengeOpen::default())
        .manage(voice_activation::VoiceActivationState::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // Every local file path resolves through this one store, so it has
            // to exist before anything that reads or writes a file.
            app.manage(AppFiles::from_app(app.handle()));
            // Usage writes happen off the Dictation Workflow path (ADR-0025), so
            // the queue that carries them has to exist before the first segment.
            let usage =
                slugtale_lib::UsageQueue::start(commands::usage::usage_writer(app.handle().clone()))
                    .map_err(std::io::Error::other)?;
            // The dictation lifecycle host owns its own state; it is managed
            // here, before the hotkey worker starts, so every activation input
            // finds it in place. The Dictation Segment worker reaches the same
            // host from its own thread, so the runtime is handed a second handle
            // on it rather than a copy of anything the host already answers.
            let host: Arc<DictationHost> = Arc::new(DictationHost::new(
                Arc::new(commands::dictation::TauriSurface {
                    app: app.handle().clone(),
                }),
                usage,
            ));
            app.manage(Arc::clone(&host));
            slugtale_lib::setup_tray(app)?;
            // The Dictation Segment worker outlives every dictation: it is what
            // keeps segments landing in the order they were spoken.
            // The runtime probes the capture ring's voiced-sample watermark at
            // the moment a Pause Flush is due — the microphone half of the
            // watermark cut (ADR-0026).
            let watermark_host = Arc::clone(&host);
            let runtime_host = Arc::clone(&host);
            let runtime = slugtale_lib::DictationRuntime::start(runtime_host, move || {
                watermark_host.voice_watermark()
            })
            .map_err(std::io::Error::other)?;
            host.set_runtime(Arc::new(runtime))
                .map_err(std::io::Error::other)?;
            // The hotkey worker starts last: from here on every activation
            // input finds both the host and the runtime in place, so a press
            // during setup cannot hit DictationHost::runtime()'s
            // "dictation runtime started" panic.
            setup_configured_hotkey(app)?;
            // Reconcile the OS login item with the stored preference so a moved or
            // rebuilt app (dev binaries change path) does not drift out of sync.
            // Launch at Login is informational and optional (slugtale-9bx): it
            // is not a Dictation Readiness item, and this reconciliation never
            // blocks dictation when the preference is off.
            let settings = commands::load_current_settings(app.handle());
            let _ =
                commands::settings::set_launch_at_login_state(app.handle(), settings.launch_at_login);
            if let Ok(model_manager) = commands::app_files(app.handle()).model_manager() {
                app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                    .set_model_manager(model_manager);
            }
            commands::warm_effective_primary_engine(app.handle());
            // Prepare Audio Capture while idle so the first Hotkey does not pay
            // for device discovery and ring allocation (slugtale-g1o.3). Only
            // when the microphone permission is already granted: preparation
            // must never prompt, and a denied microphone stays on the normal
            // permission path.
            if commands::platform::CurrentPlatform::new().microphone_granted() {
                commands::dictation::dictation_host(app.handle()).prepare_capture(&settings);
            }
            // Voice Activation is opt-in: the always-on listener only starts
            // when a previously saved preference asks for it (slugtale-e95).
            if let Err(error) = voice_activation::sync_worker(
                app.handle(),
                commands::load_current_settings(app.handle()).voice_activation_enabled,
            ) {
                eprintln!("voice activation worker did not start: {error}");
            }
            if reauthorize_permissions {
                slugtale_lib::show_settings(app.handle().clone());
                #[cfg(target_os = "macos")]
                slugtale_lib::request_microphone_access().map_err(std::io::Error::other)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            let Some(label) = WindowLabel::from_label(window.label()) else {
                return;
            };
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } if label.hides_on_close() => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // The Typing Challenge window can also be closed from its title
                // bar, which never reaches the close command. Either way, the
                // hotkey has to start working again the moment the window goes.
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                    if label == WindowLabel::TypingChallenge =>
                {
                    commands::usage::mark_typing_challenge_closed(window);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings_readiness,
            get_settings,
            dictation_bar_displays,
            open_microphone_settings,
            open_text_insertion_settings,
            save_hotkey_settings,
            save_transcription_settings,
            save_transcript_cleanup_settings,
            save_microphone_settings,
            voice_activation_supported,
            save_voice_activation_settings,
            save_dictation_bar_settings,
            dictation_bar_pointer_over,
            save_launch_at_login,
            check_for_app_update,
            open_app_update_release,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcription_engines,
            set_transcription_engines,
            install_engine_assets,
            remove_engine_assets,
            dictation_event,
            get_usage_summary,
            set_usage_storing,
            set_typing_estimate,
            get_typing_challenge,
            submit_typing_challenge,
            redo_typing_challenges,
            open_typing_challenge,
            close_typing_challenge
        ])
        .build(tauri::generate_context!())
        .expect("error while building Slugtale");

    // `App::run` terminates with `process::exit`, which skips Rust destructors.
    // Use the returning event loop and explicitly quiesce/drop Whisper first so
    // ggml's C++ Metal globals never tear down around live resources (p1u).
    let exit_code = app.run_return(|app, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .shutdown();
        }
    });

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
