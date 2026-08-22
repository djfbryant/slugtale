//! Platform Adapter execution for one Dictation Segment.
//!
//! The Dictation Workflow stays platform-neutral. This module owns the OS work
//! immediately before Text Insertion: remembering and restoring the text
//! target, platform notices, and construction of the insertion/rescue pair.

use crate::{InsertionRescue, TextInsertion};

fn focus_settle_delay(activated: bool) -> Option<std::time::Duration> {
    activated.then_some(std::time::Duration::from_millis(120))
}

pub fn capture_text_target() -> Option<i32> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        return crate::frontmost_app_pid();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

/// Prepare the current Platform Adapter for immediate Text Insertion.
///
/// Focus restoration deliberately repeats for every Dictation Segment. This is
/// the ADR-0015 rule that makes a Pause Flush behave like ordinary Immediate
/// Insertion at the current caret.
pub fn prepare_text_insertion(
    target: Option<i32>,
) -> Result<(Box<dyn TextInsertion>, Box<dyn InsertionRescue>), String> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    if let Some(pid) = target {
        if crate::activate_app(pid) {
            std::thread::sleep(
                focus_settle_delay(true).expect("successful activation has a settle delay"),
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        if !crate::accessibility_trusted() {
            let _ = crate::notify(
                "Slugtale needs Accessibility access",
                "Turn on Slugtale under System Settings → Privacy & Security → Accessibility so it can type into other apps. Until then your transcription is copied to the clipboard — paste it with Cmd+V.",
            );
        }
        return Ok((
            Box::new(crate::MacosTextInsertion::new()),
            Box::new(crate::MacosInsertionRescue::new()),
        ));
    }

    #[cfg(target_os = "windows")]
    {
        return Ok((
            Box::new(crate::WindowsTextInsertion::new()),
            Box::new(crate::WindowsInsertionRescue::new()),
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if !crate::detect_session().is_supported() {
            let _ = crate::notify(
                "Slugtale needs an X11 session",
                "Slugtale currently types into other apps only on an X11 session. Until you switch to X11 your transcription is copied to the clipboard — paste it with Ctrl+V.",
            );
        }
        return Ok((
            Box::new(crate::LinuxTextInsertion::new()),
            Box::new(crate::LinuxInsertionRescue::new()),
        ));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = target;
        Err("text insertion is not implemented for this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_only_waits_after_a_successful_activation() {
        assert_eq!(focus_settle_delay(false), None);
        assert_eq!(
            focus_settle_delay(true),
            Some(std::time::Duration::from_millis(120))
        );
    }
}
