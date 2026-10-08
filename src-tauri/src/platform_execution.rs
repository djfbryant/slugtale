//! Platform Adapter execution for one Dictation Segment.
//!
//! The Dictation Workflow stays platform-neutral. This module owns the OS work
//! immediately before Text Insertion: the Dictation Session that is still allowed
//! to type, the text target the words belong to, platform notices, and
//! construction of the insertion/rescue pair.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{InsertionRescue, TextInsertion};

/// How long the target application needs after activation before it is safe to
/// type into. Unchanged from the previous always-sleep behaviour; what changed
/// is *when* the clock runs (slugtale-g1o.1).
pub const FOCUS_SETTLE_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

/// The two operating-system questions Text Insertion asks about the app it is
/// about to type into. A trait rather than a direct call so the target policy is
/// exercised by injected outcomes in tests, with no desktop session involved
/// (slugtale-7lq0).
pub trait TextTargetFocus: Send + Sync {
    /// The process id of the app that owns the keyboard focus right now.
    fn frontmost_app(&self) -> Option<i32>;

    /// Bring `pid` back to the front. Reports whether the OS accepted it.
    fn activate(&self, pid: i32) -> bool;
}

/// The real operating system, which is what production asks.
pub struct SystemTextTargetFocus;

impl TextTargetFocus for SystemTextTargetFocus {
    fn frontmost_app(&self) -> Option<i32> {
        capture_text_target()
    }

    fn activate(&self, pid: i32) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        {
            crate::activate_app(pid)
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = pid;
            false
        }
    }
}

/// The last question asked before the first keystroke: is the Dictation Session
/// this segment belongs to still the one the user is running? Answered by the
/// Dictation Host, which owns session identity; `None` on a prepared pair means
/// nothing gates it.
pub type InsertionGate = Arc<dyn Fn() -> bool + Send + Sync>;

/// A focus that never drifts, for a wrapper built outside `prepare_text_insertion`
/// (tests only), so the settlement cases need no scripted operating system.
#[cfg(test)]
struct AlwaysFocused;

#[cfg(test)]
impl AlwaysFocused {
    const PID: i32 = 42;
}

#[cfg(test)]
impl TextTargetFocus for AlwaysFocused {
    fn frontmost_app(&self) -> Option<i32> {
        Some(Self::PID)
    }

    fn activate(&self, _pid: i32) -> bool {
        true
    }
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

/// When an activated target becomes safe to type into, as a deadline rather
/// than a sleep: `None` when no activation happened (nothing to wait for),
/// otherwise `now` plus [`FOCUS_SETTLE_DELAY`]. Injected-clock seam for tests.
fn settle_deadline(activated: bool, now: std::time::Instant) -> Option<std::time::Instant> {
    activated.then(|| now + FOCUS_SETTLE_DELAY)
}

/// Whether `target` is safe to type into right now.
///
/// Application identity is the whole of the target policy: Slugtale pins the app
/// the user started dictating into, not the exact text field, and re-checks that
/// app immediately before typing (slugtale-7lq0, ADR-0015). Three answers, all
/// of which a caller must respect:
///
/// - No target at all. Nothing is known about what has focus, so typing would be
///   typing into an arbitrary app. Refused; the Dictation Workflow's Insertion
///   Rescue preserves the words instead.
/// - The target still holds focus. Go ahead.
/// - The target lost focus while the model worked. It is activated once and
///   verified again; if the OS will not bring it back, the words are rescued
///   rather than delivered to whatever is in front now.
fn target_is_safe(target: Option<i32>, focus: &dyn TextTargetFocus) -> bool {
    let Some(pid) = target else {
        return false;
    };
    if focus.frontmost_app() == Some(pid) {
        return true;
    }
    focus.activate(pid) && focus.frontmost_app() == Some(pid)
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
    /// Shared with the wrapper so the caller can tell a refused insertion from
    /// an inserted one. The Dictation Workflow reports success for a segment
    /// whose adapter declined to type, and only the Dictation Host knows that
    /// the refusal means "cancelled", not "typed".
    refused: Arc<AtomicBool>,
}

impl PreparedInsertion {
    pub fn new(insertion: SettledTextInsertion, rescue: Box<dyn InsertionRescue>) -> Self {
        let refused = Arc::clone(&insertion.refused);
        Self {
            insertion,
            rescue,
            refused,
        }
    }

    /// Ask `gate` before this segment types anything, so a dictation the user
    /// cancelled — or one a newer Start has replaced — cannot deliver words
    /// through an insertion that is already running (slugtale-cbxb).
    pub fn guard_insertion_with(&mut self, gate: InsertionGate) {
        self.insertion.gate = Some(gate);
    }

    /// Whether the gate refused this segment at the insertion boundary.
    pub fn insertion_was_refused(&self) -> bool {
        self.refused.load(Ordering::SeqCst)
    }
}

/// A text insertion adapter that enforces its target's settlement deadline
/// before the first keystroke. Target knowledge — which app was activated and
/// when it became safe — stays inside this Platform Adapter type; the Dictation
/// Workflow only sees a plain [`TextInsertion`].
pub struct SettledTextInsertion {
    inner: Box<dyn TextInsertion>,
    ready_at: Option<std::time::Instant>,
    /// The app this segment's words belong to, pinned when the dictation began.
    target: Option<i32>,
    focus: Arc<dyn TextTargetFocus>,
    /// The Dictation Session gate, when the Dictation Host has installed one.
    gate: Option<InsertionGate>,
    /// Set by the gate above. See [`PreparedInsertion::insertion_was_refused`].
    refused: Arc<AtomicBool>,
}

impl SettledTextInsertion {
    /// Wrap `inner` so its first keystroke waits for `ready_at`. `ready_at` of
    /// `None` means the text target never moved, so nothing is owed. Test-only:
    /// production always goes through [`prepare_text_insertion`], which pins a
    /// real target and the real operating system.
    #[cfg(test)]
    pub(crate) fn new(inner: Box<dyn TextInsertion>, ready_at: Option<std::time::Instant>) -> Self {
        Self::targeted(
            inner,
            ready_at,
            Some(AlwaysFocused::PID),
            Arc::new(AlwaysFocused),
            None,
        )
    }

    fn targeted(
        inner: Box<dyn TextInsertion>,
        ready_at: Option<std::time::Instant>,
        target: Option<i32>,
        focus: Arc<dyn TextTargetFocus>,
        gate: Option<InsertionGate>,
    ) -> Self {
        Self {
            inner,
            ready_at,
            target,
            focus,
            gate,
            refused: Arc::new(AtomicBool::new(false)),
        }
    }

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
    ) -> Result<(), crate::TextInsertionError> {
        let remaining = self.settle_remaining_at(std::time::Instant::now());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }

        // Both questions are asked here, at the boundary where the words would
        // reach the user's document: the decode that precedes it can take
        // seconds, which is long enough for the user to cancel this dictation or
        // start another one.
        if let Some(gate) = self.gate.as_ref() {
            if !gate() {
                self.refused.store(true, Ordering::SeqCst);
                return Ok(());
            }
        }
        if !target_is_safe(self.target, self.focus.as_ref()) {
            return Err(crate::TextInsertionError::new(
                "the dictation's text target could not be confirmed",
            ));
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
/// Transcription (slugtale-g1o.1). The check that the target is really back
/// happens in [`SettledTextInsertion::insert`], just before typing.
pub fn prepare_text_insertion(target: Option<i32>) -> Result<PreparedInsertion, String> {
    prepare_text_insertion_with(target, Arc::new(SystemTextTargetFocus))
}

/// [`prepare_text_insertion`] with the operating system injected.
pub fn prepare_text_insertion_with(
    target: Option<i32>,
    focus: Arc<dyn TextTargetFocus>,
) -> Result<PreparedInsertion, String> {
    let mut ready_at = None;

    if let Some(pid) = target {
        // Activate now, start the settlement clock now, and do not sleep:
        // Transcription runs during the window instead of after it.
        if focus.activate(pid) {
            ready_at = settle_deadline(true, std::time::Instant::now());
        }
    }

    let insertion = make_text_insertion()?;

    Ok(PreparedInsertion::new(
        SettledTextInsertion::targeted(
            insertion,
            ready_at,
            target,
            Arc::clone(&focus),
            None,
        ),
        make_insertion_rescue(),
    ))
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
        unreachable!(
            "prepare_text_insertion errors before reaching the rescue on unsupported platforms"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn transcription_shorter_than_the_window_leaves_settlement_to_wait() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        // Transcription finished 40 ms in; 80 ms of the window is still owed.
        let remaining =
            deadline.saturating_duration_since(started + std::time::Duration::from_millis(40));

        assert_eq!(remaining, std::time::Duration::from_millis(80));
    }

    #[test]
    fn transcription_exactly_the_window_owes_no_settlement() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        let remaining = deadline.saturating_duration_since(started + FOCUS_SETTLE_DELAY);

        assert_eq!(remaining, std::time::Duration::ZERO);
    }

    #[test]
    fn transcription_longer_than_the_window_owes_no_settlement() {
        let started = std::time::Instant::now();
        let deadline = settle_deadline(true, started).expect("activated");

        let remaining = deadline.saturating_duration_since(started + FOCUS_SETTLE_DELAY * 10);

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
        let insertion = SettledTextInsertion::new(Box::new(UnreachableInsertion), None);

        assert_eq!(
            insertion.settle_remaining_at(std::time::Instant::now()),
            std::time::Duration::ZERO
        );
    }

    #[test]
    fn capture_text_target_reports_the_frontmost_application() {
        let pid = capture_text_target();
        #[cfg(target_os = "linux")]
        {
            // Only an X11 session with a reachable X server can answer, and a
            // headless runner has neither. `None` is the honest answer there,
            // not a failure of the X11 path.
            let has_x_server = std::env::var("DISPLAY").is_ok_and(|value| !value.is_empty());
            if has_x_server && crate::detect_session().is_supported() {
                assert!(
                    pid.is_some(),
                    "an X11 session with a display has a frontmost app"
                );
            } else {
                assert_eq!(pid, None, "no display server session means no text target");
            }
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(pid.is_some(), "a desktop session has a frontmost app");
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        assert_eq!(pid, None);
    }

    #[test]
    fn settled_insertion_waits_before_inserting_when_settlement_is_still_owed() {
        let started = std::time::Instant::now();
        let insertion = SettledTextInsertion::new(
            Box::new(RecordingInsertion(Arc::new(Mutex::new(Vec::new())))),
            Some(started + FOCUS_SETTLE_DELAY),
        );

        insertion
            .insert(&crate::FinalTranscription::plain("hello".to_string()))
            .unwrap();

        assert!(
            started.elapsed() >= FOCUS_SETTLE_DELAY,
            "insert must honour the settlement window"
        );
    }

    // ---- the text target just before typing (slugtale-7lq0) ----

    /// The operating system a test scripts: what holds focus, whether it
    /// accepts an activation request, and what an accepted activation actually
    /// brings forward. Injected, so the target policy is decided here rather
    /// than by whatever happens to be in front on the machine running the tests.
    #[derive(Default)]
    struct FakeFocus {
        frontmost: Mutex<Option<i32>>,
        accepts_activation: bool,
        /// What an accepted activation puts in front. `Some(None)` is the OS
        /// accepting the request without the app ever coming back.
        restores_to: Option<Option<i32>>,
        activations: Mutex<Vec<i32>>,
    }

    impl FakeFocus {
        /// The pinned target still holds focus.
        fn holding(pid: i32) -> Arc<Self> {
            Arc::new(Self {
                frontmost: Mutex::new(Some(pid)),
                ..Default::default()
            })
        }

        /// Focus moved elsewhere and the OS brings `target` back.
        fn restoring(pid: i32, target: i32) -> Arc<Self> {
            Arc::new(Self {
                frontmost: Mutex::new(Some(pid)),
                accepts_activation: true,
                restores_to: Some(Some(target)),
                activations: Mutex::new(Vec::new()),
            })
        }

        /// Focus moved elsewhere and the OS refuses to bring `target` back.
        fn refusing(pid: i32) -> Arc<Self> {
            Arc::new(Self {
                frontmost: Mutex::new(Some(pid)),
                accepts_activation: false,
                ..Default::default()
            })
        }

        /// Focus moved elsewhere; the OS accepts the request and brings nothing
        /// forward.
        fn accepting_without_restoring(pid: i32) -> Arc<Self> {
            Arc::new(Self {
                frontmost: Mutex::new(Some(pid)),
                accepts_activation: true,
                restores_to: Some(None),
                activations: Mutex::new(Vec::new()),
            })
        }

        fn activations(&self) -> Vec<i32> {
            self.activations.lock().unwrap().clone()
        }
    }

    impl TextTargetFocus for FakeFocus {
        fn frontmost_app(&self) -> Option<i32> {
            *self.frontmost.lock().unwrap()
        }

        fn activate(&self, pid: i32) -> bool {
            self.activations.lock().unwrap().push(pid);
            if !self.accepts_activation {
                return false;
            }
            if let Some(restored) = self.restores_to {
                *self.frontmost.lock().unwrap() = restored;
            }
            true
        }
    }

    /// A prepared pair whose insertion only reaches a real keystroke while
    /// `session_is_live`. `inner` decides what a real keystroke would do, so a
    /// test can prove it never happened.
    fn gated(
        inner: Box<dyn TextInsertion>,
        focus: Arc<dyn TextTargetFocus>,
        target: Option<i32>,
        session_is_live: bool,
    ) -> PreparedInsertion {
        let insertion =
            SettledTextInsertion::targeted(inner, None, target, focus, None);
        let mut prepared = PreparedInsertion::new(insertion, Box::new(UnreachableRescue));
        prepared.guard_insertion_with(Arc::new(move || session_is_live));
        prepared
    }

    #[test]
    fn a_target_that_kept_the_focus_is_typed_into_without_another_activation() {
        // Nothing to restore, so the target is confirmed by one question and no
        // second activation is asked for on the way in.
        let focus = FakeFocus::holding(7);

        assert!(target_is_safe(Some(7), focus.as_ref()));
        assert!(focus.activations().is_empty());
    }

    #[test]
    fn a_target_that_lost_the_focus_is_restored_and_verified_before_typing() {
        let focus = FakeFocus::restoring(9, 7);

        assert!(target_is_safe(Some(7), focus.as_ref()));
        assert_eq!(focus.activations(), [7]);
    }

    #[test]
    fn a_target_the_os_cannot_bring_back_is_never_typed_into() {
        // The OS refused to restore it, so the words must not go to whatever is
        // in front now. The refusal surfaces as an insertion failure, which is
        // what sends the Dictation Workflow to the Insertion Rescue.
        let focus = FakeFocus::refusing(9);

        assert!(!target_is_safe(Some(7), focus.as_ref()));
        assert_eq!(focus.activations(), [7]);
    }

    #[test]
    fn an_activation_the_os_accepts_but_does_not_perform_is_not_believed() {
        // Accepting the request is not the app coming back; only the re-check
        // afterwards decides.
        let focus = FakeFocus::accepting_without_restoring(9);

        assert!(!target_is_safe(Some(7), focus.as_ref()));
    }

    #[test]
    fn a_missing_target_is_never_typed_into_rather_than_into_whatever_has_focus() {
        let focus = FakeFocus::holding(9);

        assert!(!target_is_safe(None, focus.as_ref()));
        assert!(
            focus.activations().is_empty(),
            "with no app pinned there is nothing to restore"
        );
    }

    #[test]
    fn a_cancelled_session_types_nothing_at_the_insertion_boundary() {
        // The gate is the cancellation boundary. It declines the keystroke, so
        // the adapter reports success — the Dictation Workflow's only signal —
        // while recording the refusal for the one caller that knows what it
        // meant: nothing was typed and nothing was rescued.
        let prepared = gated(
            Box::new(UnreachableInsertion),
            FakeFocus::holding(7),
            Some(7),
            false,
        );

        prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("cancelled words".to_string()))
            .expect("a declined insertion is not a failure");

        assert!(prepared.insertion_was_refused());
    }

    #[test]
    fn a_live_session_passes_the_gate_and_types_into_its_confirmed_target() {
        let focus = FakeFocus::holding(7);
        let typed = Arc::new(Mutex::new(Vec::new()));
        let prepared = gated(
            Box::new(RecordingInsertion(Arc::clone(&typed))),
            Arc::clone(&focus) as Arc<dyn TextTargetFocus>,
            Some(7),
            true,
        );

        prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("typed words".to_string()))
            .unwrap();

        assert!(!prepared.insertion_was_refused());
        assert_eq!(typed.lock().unwrap().len(), 1, "the words reached the target");
        assert!(focus.activations().is_empty());
    }

    #[test]
    fn a_live_session_whose_target_is_gone_reports_an_insertion_failure() {
        // The gate passed, but the app the words belong to cannot be confirmed.
        // An error here is what makes the Dictation Workflow preserve the words
        // on the clipboard rather than lose them.
        let focus = FakeFocus::refusing(9);
        let prepared = gated(Box::new(UnreachableInsertion), focus, Some(7), true);

        let error = prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("rescued words".to_string()))
            .expect_err("an unconfirmable target must not be typed into");

        assert!(
            error.to_string().contains("text target could not be confirmed"),
            "unexpected error: {error}"
        );
        assert!(!prepared.insertion_was_refused());
    }

    /// Records the text a real keystroke would have delivered.
    struct RecordingInsertion(Arc<Mutex<Vec<String>>>);

    impl TextInsertion for RecordingInsertion {
        fn insert(
            &self,
            transcription: &crate::FinalTranscription,
        ) -> Result<(), crate::TextInsertionError> {
            self.0.lock().unwrap().push(transcription.text.clone());
            Ok(())
        }
    }

    struct UnreachableInsertion;

    impl TextInsertion for UnreachableInsertion {
        fn insert(
            &self,
            _transcription: &crate::FinalTranscription,
        ) -> Result<(), crate::TextInsertionError> {
            panic!("test never inserts");
        }
    }

    struct UnreachableRescue;

    impl InsertionRescue for UnreachableRescue {
        fn rescue(
            &self,
            _transcription: &crate::FinalTranscription,
        ) -> Result<(), crate::InsertionRescueError> {
            panic!("test never rescues");
        }
    }
}
