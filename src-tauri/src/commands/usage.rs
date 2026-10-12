
use std::sync::Arc;

use tauri::Emitter;

use slugtale_lib::{TypingChallengeOpen, WindowLabel};

use super::platform::locale_week_start;
use super::{app_files, load_current_settings, update_current_settings};

#[derive(serde::Serialize)]
pub(crate) struct UsageSpan {
    dictations: u32,
    words: u32,
    time_saved: Option<String>,
}

fn usage_span(totals: &slugtale_lib::UsageTotals, words_per_minute: Option<u32>) -> UsageSpan {
    let seconds = slugtale_lib::time_saved_seconds(totals, words_per_minute);
    UsageSpan {
        dictations: totals.dictations,
        words: totals.words,
        time_saved: seconds.map(|seconds| slugtale_lib::format_time_saved(Some(seconds))),
    }
}

#[derive(serde::Serialize)]
pub(crate) struct UsageSummary {
    store_usage: bool,
    today: UsageSpan,
    this_week: UsageSpan,
    all_time: UsageSpan,
    measured_wpm: Option<u32>,
    typed_estimate: Option<u32>,
    completed_challenges: usize,
    challenge_count: usize,
}

#[tauri::command]
pub(crate) fn get_usage_summary(app: tauri::AppHandle) -> UsageSummary {
    let settings = load_current_settings(&app);
    let baseline = &settings.typing_baseline;
    let words_per_minute = baseline.effective_wpm();
    let usage = if settings.store_usage {
        app_files(&app).usage()
    } else {
        slugtale_lib::UsageFile::default()
    };
    let today = slugtale_lib::today_local();
    let week_start = locale_week_start();

    UsageSummary {
        store_usage: settings.store_usage,
        today: usage_span(
            &slugtale_lib::totals_for_day(&usage, today),
            words_per_minute,
        ),
        this_week: usage_span(
            &slugtale_lib::totals_for_week(&usage, today, week_start),
            words_per_minute,
        ),
        all_time: usage_span(&slugtale_lib::totals_all_time(&usage), words_per_minute),
        measured_wpm: baseline.measured_wpm(),
        typed_estimate: baseline.typed_estimate,
        completed_challenges: baseline.completed_challenges(),
        challenge_count: slugtale_lib::TYPING_CHALLENGE_COUNT,
    }
}

/// Turn storing Daily Usage Records on or off.
///
/// Turning it off deletes the Usage File outright rather than leaving it to rot
/// unread: "stop storing this" has to mean the stored thing is gone. The Typing
/// Baseline is in the Settings File and is untouched.
#[tauri::command]
pub(crate) fn set_usage_storing(app: tauri::AppHandle, enabled: bool) -> Result<UsageSummary, String> {
    app_files(&app).set_usage_storing(enabled)?;

    Ok(get_usage_summary(app))
}

/// Set or clear the typed typing-speed estimate. Refused once the three Typing
/// Challenges have produced a measurement.
#[tauri::command]
pub(crate) fn set_typing_estimate(
    app: tauri::AppHandle,
    estimate: Option<u32>,
) -> Result<UsageSummary, String> {
    update_current_settings(&app, |settings| {
        slugtale_lib::apply_typed_estimate(&mut settings.typing_baseline, estimate)
            .map_err(|error| error.to_string())
    })?;

    Ok(get_usage_summary(app))
}

#[derive(serde::Serialize)]
pub(crate) struct TypingChallengeState {
    passage: Option<String>,
    passage_index: Option<usize>,
    completed: usize,
    total: usize,
    seconds: u32,
    measured_wpm: Option<u32>,
}

fn typing_challenge_state(baseline: &slugtale_lib::TypingBaseline) -> TypingChallengeState {
    let passage_index = baseline.next_passage_index();
    TypingChallengeState {
        passage: passage_index.map(|index| {
            slugtale_lib::TYPING_CHALLENGE_PASSAGES[index]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }),
        passage_index,
        completed: baseline.completed_challenges(),
        total: slugtale_lib::TYPING_CHALLENGE_COUNT,
        seconds: slugtale_lib::TYPING_CHALLENGE_SECONDS,
        measured_wpm: baseline.measured_wpm(),
    }
}

#[tauri::command]
pub(crate) fn get_typing_challenge(app: tauri::AppHandle) -> TypingChallengeState {
    typing_challenge_state(&load_current_settings(&app).typing_baseline)
}

/// Score one finished Typing Challenge and store it.
///
/// The window sends the text as it finally stood, so backspacing is free — which
/// is how people type, and the point is to measure that.
#[tauri::command]
pub(crate) fn submit_typing_challenge(
    app: tauri::AppHandle,
    passage_index: usize,
    typed: String,
) -> Result<TypingChallengeState, String> {
    let passage = slugtale_lib::TYPING_CHALLENGE_PASSAGES
        .get(passage_index)
        .ok_or_else(|| format!("there is no typing challenge passage {passage_index}"))?;
    let words_per_minute = slugtale_lib::score_typing_challenge(
        passage,
        &typed,
        slugtale_lib::TYPING_CHALLENGE_SECONDS,
    );

    let settings = update_current_settings(&app, |settings| {
        slugtale_lib::record_typing_challenge(
            &mut settings.typing_baseline,
            passage_index,
            words_per_minute,
        );
        Ok(())
    })?;

    notify_usage_changed(&app);
    Ok(typing_challenge_state(&settings.typing_baseline))
}

#[tauri::command]
pub(crate) fn redo_typing_challenges(
    app: tauri::AppHandle,
) -> Result<TypingChallengeState, String> {
    let settings = update_current_settings(&app, |settings| {
        slugtale_lib::redo_typing_challenges(&mut settings.typing_baseline);
        Ok(())
    })?;

    notify_usage_changed(&app);
    Ok(typing_challenge_state(&settings.typing_baseline))
}

pub(crate) fn mark_typing_challenge_open(manager: &impl tauri::Manager<tauri::Wry>) {
    manager.state::<TypingChallengeOpen>().set(true);
}

/// The Typing Challenge window has gone — closed by its own command, by its title
/// bar, or never having opened at all — so the dictation Hotkey works again. Every
/// path out of the window comes through here.
pub(crate) fn mark_typing_challenge_closed(manager: &impl tauri::Manager<tauri::Wry>) {
    manager.state::<TypingChallengeOpen>().set(false);
}

#[tauri::command]
pub(crate) fn open_typing_challenge(app: tauri::AppHandle) -> Result<(), String> {
    mark_typing_challenge_open(&app);

    if let Some(window) = WindowLabel::TypingChallenge.window(&app) {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let built = tauri::WebviewWindowBuilder::new(
        &app,
        WindowLabel::TypingChallenge.as_str(),
        tauri::WebviewUrl::App("typing-challenge.html".into()),
    )
    .title("Slugtale Typing Challenge")
    .inner_size(760.0, 620.0)
    .resizable(false)
    .build();

    match built {
        Ok(_) => Ok(()),
        Err(error) => {
            mark_typing_challenge_closed(&app);
            Err(error.to_string())
        }
    }
}

#[tauri::command]
pub(crate) fn close_typing_challenge(app: tauri::AppHandle) -> Result<(), String> {
    mark_typing_challenge_closed(&app);
    if let Some(window) = WindowLabel::TypingChallenge.window(&app) {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn notify_usage_changed(app: &tauri::AppHandle) {
    if let Some(window) = WindowLabel::Settings.window(app) {
        let _ = window.emit("usage-changed", ());
    }
}

pub(crate) fn usage_writer(app: tauri::AppHandle) -> Arc<slugtale_lib::UsageSink> {
    Arc::new(move |date, segment| {
        if app_files(&app).record_counted_segment(date, segment) {
            notify_usage_changed(&app);
        }
    })
}
