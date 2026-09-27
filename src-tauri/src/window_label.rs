//! Window identity: the one place that names every window Slugtale owns and says
//! what closing and showing each of them means.
//!
//! A label is a wire string. It arrives from `tauri.conf.json` for the declared
//! windows, from a builder for the ones created on demand, and back out of
//! `window.label()` on every window event. Spelling one by hand at a call site
//! is a silent no-op when it is wrong: the lookup misses and the bar never
//! appears, with nothing failing. So the name lives here once and the policy that
//! follows from the name lives with it.

use tauri::{Manager, Runtime, WebviewWindow};

/// Every window Slugtale owns.
///
/// Slugtale is a Resident App (ADR-0008), so this set is closed and small: the
/// settings surface, the Dictation Bar, and the Typing Challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLabel {
    /// The settings surface, reopened from the tray.
    Settings,
    /// The Dictation Bar (CONTEXT.md). Declared in tauri.conf.json and shown for
    /// the length of a dictation.
    DictationBar,
    /// The Typing Challenge. Created on demand rather than declared in
    /// tauri.conf.json: most users never open it, and a hidden window carrying a
    /// live webview for the life of the app is a cost with no benefit.
    TypingChallenge,
}

impl WindowLabel {
    pub const SETTINGS: &'static str = "settings";
    pub const DICTATION_BAR: &'static str = "dictation-bar";
    pub const TYPING_CHALLENGE: &'static str = "typing-challenge";

    /// The whole set, so a test can walk it instead of repeating it.
    pub const ALL: [WindowLabel; 3] = [
        WindowLabel::Settings,
        WindowLabel::DictationBar,
        WindowLabel::TypingChallenge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WindowLabel::Settings => WindowLabel::SETTINGS,
            WindowLabel::DictationBar => WindowLabel::DICTATION_BAR,
            WindowLabel::TypingChallenge => WindowLabel::TYPING_CHALLENGE,
        }
    }

    /// Total: a label is one of these three or it is not one of ours. Windows
    /// Tauri opens itself, and anything a plugin adds, arrive here too, so this
    /// cannot assume it is always being asked about a Slugtale window.
    pub fn from_label(label: &str) -> Option<WindowLabel> {
        match label {
            WindowLabel::SETTINGS => Some(WindowLabel::Settings),
            WindowLabel::DICTATION_BAR => Some(WindowLabel::DictationBar),
            WindowLabel::TYPING_CHALLENGE => Some(WindowLabel::TypingChallenge),
            _ => None,
        }
    }

    /// This window, if it exists yet. `None` is the normal answer for the two
    /// windows that are hidden or not built.
    pub fn window<R: Runtime>(self, manager: &impl Manager<R>) -> Option<WebviewWindow<R>> {
        manager.get_webview_window(self.as_str())
    }

    /// Whether closing this window hides it rather than destroying it.
    ///
    /// Slugtale is a tray Resident App (ADR-0008): the settings window is
    /// reopened from the tray, so closing it must hide it — destroying it both
    /// kills the only reopen path and, as the last window, would quit the whole
    /// app. The Dictation Bar is hidden and shown by the dictation lifecycle, and
    /// the Typing Challenge is created on demand, so neither is worth keeping
    /// alive behind a hide (ADR-0025).
    pub fn hides_on_close(self) -> bool {
        matches!(self, WindowLabel::Settings)
    }

    /// Whether showing this window may take keyboard focus from the Text Target
    /// the user was already typing into.
    ///
    /// The Dictation Bar is the exception and the reason it is worth naming: it
    /// appears over somebody else's window mid-sentence, so it shows without
    /// focus and the keystrokes keep landing where the user left them.
    pub fn takes_focus(self) -> bool {
        !matches!(self, WindowLabel::DictationBar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_round_trips_through_its_label() {
        for label in WindowLabel::ALL {
            assert_eq!(WindowLabel::from_label(label.as_str()), Some(label));
        }
    }

    #[test]
    fn a_label_that_is_not_ours_is_not_a_window() {
        // Tauri opens windows of its own and plugins add more, so an unrecognised
        // label has to be answerable rather than a panic.
        assert_eq!(WindowLabel::from_label("main"), None);
        assert_eq!(WindowLabel::from_label(""), None);
    }

    #[test]
    fn the_settings_window_hides_instead_of_closing() {
        assert!(WindowLabel::Settings.hides_on_close());
    }

    #[test]
    fn the_other_windows_are_destroyed_by_a_close_request() {
        // The Typing Challenge is created on demand and most users never open it,
        // so keeping a live webview around for the life of the app would be a
        // cost with no benefit. Closing it also has to actually end the run in
        // progress, not park a half-typed passage behind a hidden window
        // (ADR-0025).
        assert!(!WindowLabel::DictationBar.hides_on_close());
        assert!(!WindowLabel::TypingChallenge.hides_on_close());
    }

    #[test]
    fn only_the_dictation_bar_preserves_the_active_text_target_focus() {
        assert!(!WindowLabel::DictationBar.takes_focus());
        assert!(WindowLabel::Settings.takes_focus());
        assert!(WindowLabel::TypingChallenge.takes_focus());
    }

    #[test]
    fn the_declared_window_labels_are_the_ones_the_code_asks_for() {
        // tauri.conf.json names the windows that exist at startup; the code finds
        // them by these labels. A rename on either side that misses the other is a
        // window that silently never appears.
        let config = std::fs::read_to_string("tauri.conf.json").expect("tauri.conf.json exists");
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let declared: Vec<&str> = config["app"]["windows"]
            .as_array()
            .expect("windows are configured")
            .iter()
            .map(|window| window["label"].as_str().expect("a window label"))
            .collect();

        for label in [WindowLabel::Settings, WindowLabel::DictationBar] {
            assert!(
                declared.contains(&label.as_str()),
                "tauri.conf.json does not configure {}",
                label.as_str()
            );
        }
        assert!(
            !declared.contains(&WindowLabel::TypingChallenge.as_str()),
            "the Typing Challenge is built on demand, not declared: {declared:?}"
        );
    }
}
