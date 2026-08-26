//! Platform Adapter execution for one Dictation Segment.
//!
//! The Dictation Workflow stays platform-neutral. This module owns the OS work
//! immediately before Text Insertion: remembering and restoring the text
//! target, platform notices, and construction of the insertion/rescue pair.

use crate::{InsertionRescue, TextInsertion};

/// How long the target application needs after activation before it is safe to
/// type into. Unchanged from the previous always-sleep behaviour; what changed
/// is *when* the clock runs (slugtale-g1o.1).
pub const FOCUS_SETTLE_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

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

/// When an activated target becomes safe to type into, as a deadline rather
/// than a sleep: `None` when no activation happened (nothing to wait for),
/// otherwise `now` plus [`FOCUS_SETTLE_DELAY`]. Injected-clock seam for tests.
fn settle_deadline(activated: bool, now: std::time::Instant) -> Option<std::time::Instant> {
    activated.then(|| now + FOCUS_SETTLE_DELAY)
}

/// The prepared text-insertion pair for one Dictation Segment.
///
/// `prepare_text_insertion` activates the segment's text target immediately and
/// records when that target will have settled, instead of sleeping for the
/// fixed settlement window on the spot. The Dictation Workflow therefore starts
/// Transcription right away; the wait, if any is still owed, happens inside
/// [`SettledTextInsertion::insert`] immediately before typing — and when
/// Transcription took at least the 120 ms settlement window, nothing waits at
/// all (slugtale-g1o.1).
pub struct PreparedInsertion {
    pub insertion: SettledTextInsertion,
    pub rescue: Box<dyn InsertionRescue>,
}

/// A text insertion adapter that enforces its target's settlement deadline
/// before the first keystroke. Target knowledge — which app was activated and
/// when it became safe — stays inside this Platform Adapter type; the Dictation
/// Workflow only sees a plain [`TextInsertion`].
pub struct SettledTextInsertion {
    inner: Box<dyn TextInsertion>,
    ready_at: Option<std::time::Instant>,
}

impl SettledTextInsertion {
    /// How much of the settlement window is still owed at `now`. Injected-clock
    /// seam for tests: shorter than the window leaves time to wait, equal or
    /// longer leaves none.
    pub fn settle_remaining_at(&self, now: std::time::Instant) -> std::time::Duration {
        match self.ready_at {
            Some(ready_at) => ready_at.saturating_duration_since(now),
            None => std::time::Duration::ZERO,
        }
    }
}

impl TextInsertion for SettledTextInsertion {
    fn insert(
        &self,
        transcription: &crate::FinalTranscription,
    ) -> Result<crate::TextInsertionOutcome, crate::TextInsertionError> {
        let remaining = self.settle_remaining_at(std::time::Instant::now());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
        self.inner.insert(transcription)
    }
}

/// Prepare the current Platform Adapter for Text Insertion into the segment's
/// target.
///
/// Focus restoration deliberately repeats for every Dictation Segment. This is
/// the ADR-0015 rule that makes a Pause Flush behave like ordinary Immediate
/// Insertion at the current caret. The activation happens here, immediately;
/// only its settlement is deferred to insert time so it can overlap
/// Transcription (slugtale-g1o.1).
pub fn prepare_text_insertion(target: Option<i32>) -> Result<PreparedInsertion, String> {
    let mut ready_at = None;

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    if let Some(pid) = target {
        // Activate now, start the settlement clock now, and do not sleep:
        // Transcription runs during the window instead of after it.
        if crate::activate_app(pid) {
            ready_at = settle_deadline(true, std::time::Instant::now());
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = target;
    }

    let insertion = make_text_insertion()?;

    Ok(PreparedInsertion {
        insertion: SettledTextInsertion { inner: insertion, ready_at },
        rescue: make_insertion_rescue(),
    })
}

fn make_text_insertion() -> Result<Box<dyn TextInsertion>, String> {
    #[cfg(target_os = "macos")]
    {
        if !crate::accessibility_trusted() {
            let _ = crate::notify(
                "Slugtale needs Accessibility access",
                "Turn on Slugtale under System Settings → Privacy & Security → Accessibility so it can type into other apps. Until then your transcription is copied to the clipboard — paste it with Cmd+V.",
            );
        }
        return Ok(Box::new(crate::MacosTextInsertion::new()));
    }

    #[cfg(target_os = "windows")]
    {
        return Ok(Box::new(crate::WindowsTextInsertion::new()));
    }

    #[cfg(target_os = "linux")]
    {
        if !crate::detect_session().is_supported() {
            let _ = crate::notify(
                "Slugtale needs an X11 session",
                "Slugtale currently types into other apps only on an X11 session. Until you switch to X11 your transcription is copied to the clipboard — paste it with Ctrl+V.",
            );
        }
        return Ok(Box::new(crate::LinuxTextInsertion::new()));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("text insertion is not implemented for this platform".to_string())
    }
}

fn make_insertion_rescue() -> Box<dyn InsertionRescue> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(crate::MacosInsertionRescue::new());
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(crate::WindowsInsertionRescue::new())
    }

    #[cfg(target_os = "linux")]
    {
        Box::new(crate::LinuxInsertionRescue::new())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        unreachable!("prepare_text_insertion errors before reaching the rescue on unsupported platforms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_shorter_than_the_window_leaves_settlement_to_wait() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        // Transcription finished 40 ms in; 80 ms of the window is still owed.
        let remaining = deadline.saturating_duration_since(started + std::time::Duration::from_millis(40));

        assert_eq!(remaining, std::time::Duration::from_millis(80));
    }

    #[test]
    fn transcription_exactly_the_window_owes_no_settlement() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        let remaining =
            deadline.saturating_duration_since(started + FOCUS_SETTLE_DELAY);

        assert_eq!(remaining, std::time::Duration::ZERO);
    }

    #[test]
    fn transcription_longer_than_the_window_owes_no_settlement() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        let remaining = deadline
            .saturating_duration_since(started + FOCUS_SETTLE_DELAY * 10);

        assert_eq!(remaining, std::time::Duration::ZERO);
    }

    #[test]
    fn a_failed_activation_never_makes_insertion_wait() {
        // No activation (or a failed one) means the target never moved: typing
        // is safe immediately, and the wrapper must not invent a delay.
        assert_eq!(settle_deadline(false, std::time::Instant::now()), None);
    }

    #[test]
    fn settled_insertion_reports_zero_remaining_without_an_activation() {
        // A SettledTextInsertion built outside prepare (tests) has no deadline.
        let insertion = SettledTextInsertion {
            inner: Box::new(UnreachableInsertion),
            ready_at: None,
        };

        assert_eq!(
            insertion.settle_remaining_at(std::time::Instant::now()),
            std::time::Duration::ZERO
        );
    }

    #[test]
    fn capture_text_target_reports_the_frontmost_application() {
        let pid = capture_text_target();
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        {
            assert!(pid.is_some(), "a desktop session should have a frontmost app");
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            assert_eq!(pid, None);
        }
    }

    #[test]
    fn settled_insertion_waits_before_inserting_when_settlement_is_still_owed() {
        let started = std::time::Instant::now();
        let insertion = SettledTextInsertion {
            inner: Box::new(RecordingInsertion),
            ready_at: Some(started + FOCUS_SETTLE_DELAY),
        };

        let outcome = insertion
            .insert(&crate::FinalTranscription::plain("hello".to_string()))
            .unwrap();

        assert_eq!(outcome, crate::TextInsertionOutcome::ClipboardFree);
        assert!(
            started.elapsed() >= FOCUS_SETTLE_DELAY,
            "insert must honour the settlement window"
        );
    }

    struct RecordingInsertion;

    impl TextInsertion for RecordingInsertion {
        fn insert(
            &self,
            _transcription: &crate::FinalTranscription,
        ) -> Result<crate::TextInsertionOutcome, crate::TextInsertionError> {
            Ok(crate::TextInsertionOutcome::ClipboardFree)
        }
    }

    struct UnreachableInsertion;

    impl TextInsertion for UnreachableInsertion {
        fn insert(
            &self,
            _transcription: &crate::FinalTranscription,
        ) -> Result<crate::TextInsertionOutcome, crate::TextInsertionError> {
            panic!("test never inserts");
        }
    }
}
