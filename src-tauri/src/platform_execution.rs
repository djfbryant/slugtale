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

/// Lets a Dictation Segment's effects run only while its Dictation Session is
/// still the one the user is running, without holding that decision open across
/// the work in between.
///
/// A plain "is it still live?" answer is not enough on its own: the words reach
/// the user through focus activation, the keystrokes and — if those fail — the
/// clipboard rescue, and a Cancel or a newer Start can land inside any of those.
/// So the guard is asked once per effect, at the effect itself, and the runtime
/// answers only once the answer cannot change underneath it. It is never held
/// across Transcription or the settling sleep (slugtale-cbxb).
pub trait SessionEffects: Send + Sync {
    /// Run `effect` only if `session` is still live, deciding and acting as one
    /// operation. Reports whether the effect ran.
    fn while_session_live(&self, session: u64, effect: &mut dyn FnMut()) -> bool;

    /// Report that the effect for `session` was refused, so the caller does not
    /// report words it never delivered.
    fn note_refused(&self, session: u64);
}

/// A guard with no session behind it, for a prepared pair nothing has scoped.
/// Every effect runs, which is what a dictation outside a session needs.
pub struct UnscopedEffects;

impl SessionEffects for UnscopedEffects {
    fn while_session_live(&self, _session: u64, effect: &mut dyn FnMut()) -> bool {
        effect();
        true
    }

    fn note_refused(&self, _session: u64) {}
}

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

    /// Scope this pair's effects to `session`, through `effects`. Every keystroke,
    /// and the clipboard rescue that may follow a failed insertion, is then
    /// decided and taken as one operation, so a Cancel or a newer Start cannot
    /// land between the decision and the effect (slugtale-cbxb).
    pub fn guard_with_session(
        &mut self,
        session: u64,
        effects: Arc<dyn SessionEffects>,
    ) {
        self.insertion.effects = Some(Arc::new(SessionScoped {
            session,
            effects: Arc::clone(&effects),
            refused: Arc::clone(&self.refused),
        }));
        self.insertion.session = session;
        self.rescue = Box::new(SessionScopedRescue {
            inner: std::mem::replace(&mut self.rescue, Box::new(UnreachableRescue)),
            session,
            scoped: Arc::clone(&effects),
            refused: Arc::clone(&self.refused),
        });
    }

    /// Whether an effect for this segment was refused because its dictation was
    /// no longer live.
    pub fn insertion_was_refused(&self) -> bool {
        self.refused.load(Ordering::SeqCst)
    }
}

/// The `SessionEffects` a prepared pair was scoped to, plus the flag its caller
/// reads afterwards.
struct SessionScoped {
    session: u64,
    effects: Arc<dyn SessionEffects>,
    refused: Arc<AtomicBool>,
}

impl SessionEffects for SessionScoped {
    fn while_session_live(&self, session: u64, effect: &mut dyn FnMut()) -> bool {
        // Only the session this pair was scoped to may be judged here; a pair
        // handed to another dictation's job must not have its own effects decided
        // by that other job's answer.
        if session != self.session {
            effect();
            return true;
        }
        if self.effects.while_session_live(session, effect) {
            true
        } else {
            self.refused.store(true, Ordering::SeqCst);
            self.effects.note_refused(session);
            false
        }
    }

    fn note_refused(&self, session: u64) {
        self.effects.note_refused(session)
    }
}

/// The Insertion Rescue of a session-scoped pair: the clipboard write is an
/// effect like any other, and a cancelled dictation must not reach the user's
/// clipboard either (slugtale-cbxb).
struct SessionScopedRescue {
    inner: Box<dyn InsertionRescue>,
    session: u64,
    scoped: Arc<dyn SessionEffects>,
    refused: Arc<AtomicBool>,
}

impl InsertionRescue for SessionScopedRescue {
    fn rescue(
        &self,
        transcription: &crate::FinalTranscription,
    ) -> Result<(), crate::InsertionRescueError> {
        let mut outcome = Ok(());
        let session = self.session;
        let ran = self.scoped.while_session_live(session, &mut || {
            if let Err(error) = self.inner.rescue(transcription) {
                outcome = Err(error);
            }
        });
        if !ran {
            self.refused.store(true, Ordering::SeqCst);
            self.scoped.note_refused(session);
            // Nothing was preserved, so this must not read to the caller as a
            // successful rescue of words the user never received anywhere.
            return Err(crate::InsertionRescueError::new(
                "the dictation was cancelled before its transcription could be preserved",
            ));
        }
        outcome
    }
}

/// A rescue that must never run, standing in while a scoped pair takes the real
/// one out of its field.
struct UnreachableRescue;

impl InsertionRescue for UnreachableRescue {
    fn rescue(
        &self,
        _transcription: &crate::FinalTranscription,
    ) -> Result<(), crate::InsertionRescueError> {
        unreachable!("the scoped rescue replaced this one")
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
    /// The Dictation Session's effect guard, when the Dictation Host has scoped
    /// this pair. Asked separately for the keystrokes and for the focus work that
    /// precedes them, because a Cancel landing between those two must stop the
    /// words as surely as one landing before either.
    effects: Option<Arc<dyn SessionEffects>>,
    /// The session those effects belong to, so the wrapper asks about its own
    /// dictation rather than whatever the caller passes down.
    session: u64,
    /// Set when an effect above was refused. See
    /// [`PreparedInsertion::insertion_was_refused`].
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
        effects: Option<Arc<dyn SessionEffects>>,
    ) -> Self {
        Self {
            inner,
            ready_at,
            target,
            focus,
            effects,
            session: 0,
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

        // The settling sleep and the focus work are both deliberately outside the
        // session guard. The sleep is 120 ms of nothing happening, and activating
        // the target is an operating-system call that can block for as long as the
        // target app takes to come forward. Holding the guard across either would
        // put the user's own Cancel in a queue behind them — and would then let
        // the words through afterwards, because the waiting Cancel would retire
        // the session only once the guard was free again (slugtale-cbxb).
        //
        // So focus is resolved first, unguarded, and the session is then asked
        // once at the boundary where the keystrokes would actually land. A Cancel
        // that arrived during the activation is therefore already recorded by the
        // time that question is asked, which is the interleaving that used to type
        // old words.
        let target_confirmed = target_is_safe(self.target, self.focus.as_ref());

        // The session is asked before anything else is reported, because a dead
        // session means this segment produces no failure at all: not a rescue, not
        // an insertion failure the caller would log.
        if !self.still_mine(|| true) {
            return Ok(());
        }
        // Only then does an unconfirmable target become an insertion failure,
        // which is what sends the Dictation Workflow to ADR-0016's rescue. The two
        // answers stay apart: conflating them would either lose the words or type
        // them where they must not go.
        if !target_confirmed {
            return Err(crate::TextInsertionError::new(
                "the dictation's text target could not be confirmed",
            ));
        }

        // The keystrokes, decided and taken as one operation.
        let mut outcome = Ok(());
        let ran = self.within_session(&mut || {
            outcome = self.inner.insert(transcription);
        });
        if !ran {
            // Refused at the boundary: the Dictation Workflow reads this as a
            // plain success, so the refusal is recorded for the caller that knows
            // what it meant, and no rescue follows a failure that never happened.
            return Ok(());
        }
        outcome
    }
}

impl SettledTextInsertion {
    /// Whether this insertion still belongs to a live dictation, asked around
    /// `work` so the answer cannot change while the work runs.
    ///
    /// The work is a `&mut dyn FnMut` because it is handed on to the guard as the
    /// closure the guard runs while holding its decision — handing over the work
    /// itself is the point, not an implementation detail.
    fn within_session(&self, work: &mut dyn FnMut()) -> bool {
        match self.effects.as_ref() {
            None => {
                work();
                true
            }
            Some(effects) => {
                let session = self.session;
                effects.while_session_live(session, work)
            }
        }
    }

    /// Run a side effect that must not happen for a dictation the user has left,
    /// reporting refusal the same way as the keystrokes. The effect's own verdict
    /// is captured rather than returned, because the answer the caller needs is
    /// whether the effect ran at all.
    fn still_mine(&self, work: impl FnOnce() -> bool) -> bool {
        let mut verdict = false;
        let mut work = Some(work);
        {
            let mut run = || verdict = work.take().expect("run once")();
            if !self.within_session(&mut run) {
                return false;
            }
        }
        verdict
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
        /// Focus has moved elsewhere and the OS will bring the target back.
        fn drifted(pid: i32) -> Self {
            Self {
                frontmost: Mutex::new(Some(pid)),
                accepts_activation: true,
                restores_to: Some(Some(pid)),
                activations: Mutex::new(Vec::new()),
            }
        }

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

    /// A prepared pair whose effects reach a real keystroke and a real rescue only
    /// while `effects` says its session is live. `inner` decides what a real
    /// keystroke would do, so a test can prove it never happened.
    fn scoped(
        inner: Box<dyn TextInsertion>,
        rescue: Box<dyn InsertionRescue>,
        focus: Arc<dyn TextTargetFocus>,
        target: Option<i32>,
        effects: Arc<dyn SessionEffects>,
    ) -> PreparedInsertion {
        let insertion = SettledTextInsertion::targeted(inner, None, target, focus, None);
        let mut prepared = PreparedInsertion::new(insertion, rescue);
        prepared.guard_with_session(TEST_SESSION, effects);
        prepared
    }

    const TEST_SESSION: u64 = 1;

    /// Session effects whose answer the test scripts, so a refusal can be forced
    /// at the boundary rather than hoped for.
    #[derive(Default)]
    struct FakeEffects {
        live: Mutex<Vec<bool>>,
        calls: Mutex<usize>,
    }

    impl FakeEffects {
        fn answering(outcomes: &[bool]) -> Arc<Self> {
            Arc::new(Self {
                live: Mutex::new(outcomes.to_vec()),
                calls: Mutex::new(0),
            })
        }

        /// How many effects were asked about, so a test can prove a refusal came
        /// from a second decision rather than from the first one.
        fn asked(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    /// A runtime host that reaches nothing, so a test can start a real
    /// [`crate::DictationRuntime`] purely for its session effects.
    struct UnscopedHost;

    impl crate::DictationRuntimeHost for UnscopedHost {
        fn take_pause_segment(&self, _session: u64, _cut: u64) -> Option<crate::CapturedAudio> {
            None
        }

        fn complete(
            &self,
            _session: u64,
            _audio: crate::CapturedAudio,
            _position: crate::DictationSegmentPosition,
        ) -> Result<crate::DictationSegmentOutcome, String> {
            Err("test host never transcribes".to_string())
        }

        fn record_counted_segment(&self, _segment: crate::CountedSegment) {}

        fn last_job_settled(&self, _session: u64) {}
    }

    impl SessionEffects for FakeEffects {
        fn while_session_live(&self, _session: u64, effect: &mut dyn FnMut()) -> bool {
            let mut live = self.live.lock().unwrap();
            *self.calls.lock().unwrap() += 1;
            let answer = live.first().copied().unwrap_or(true);
            if answer {
                effect();
            }
            // Every answer is consumed, so a scripted sequence walks the effects
            // in order: target check, then keystrokes, then any rescue.
            if !live.is_empty() {
                live.remove(0);
            }
            answer
        }

        fn note_refused(&self, _session: u64) {}
    }

    struct RecordingRescue(Arc<Mutex<Vec<String>>>);

    impl InsertionRescue for RecordingRescue {
        fn rescue(
            &self,
            transcription: &crate::FinalTranscription,
        ) -> Result<(), crate::InsertionRescueError> {
            self.0.lock().unwrap().push(transcription.text.clone());
            Ok(())
        }
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
        // Refused at the keystroke boundary, so the adapter reports success —
        // the Dictation Workflow's only signal — while recording the refusal for
        // the one caller that knows what it meant: nothing was typed.
        let prepared = scoped(
            Box::new(UnreachableInsertion),
            Box::new(UnreachableRescue),
            FakeFocus::holding(7),
            Some(7),
            FakeEffects::answering(&[true, false]),
        );

        prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("cancelled words".to_string()))
            .expect("a declined insertion is not a failure");

        assert!(prepared.insertion_was_refused());
    }

    /// An activation that tells the test when it has been entered and then
    /// waits, bounded at both ends: the "entered" signal is a channel the test
    /// receives with a deadline, and the wait ends on the release, on the test's
    /// sender being dropped, or on its own deadline. A failing test can
    /// therefore never park the insertion thread, which is what an unbounded
    /// gate used to do (slugtale-cbxb).
    struct BlockingActivation {
        entered: std::sync::mpsc::Sender<()>,
        /// Held behind a `Mutex` only because `activate` takes `&self`; the
        /// receiver's `recv_timeout` also returns once the test drops its
        /// sender, which is the cleanup path a panicking test takes.
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        focus: FakeFocus,
    }

    /// Long enough for any test to act, short enough that a mistake ends the
    /// wait instead of the suite.
    const ACTIVATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

    /// The test's end of a [`BlockingActivation`], so waiting and releasing are
    /// bounded channel operations rather than a shared flag waited on forever.
    struct ActivationHandle {
        entered: std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::Sender<()>,
    }

    impl BlockingActivation {
        fn new() -> (Arc<Self>, ActivationHandle) {
            let (entered, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release) = std::sync::mpsc::channel();
            (
                Arc::new(Self {
                    entered,
                    release: Mutex::new(release),
                    focus: FakeFocus::drifted(9),
                }),
                ActivationHandle {
                    entered: entered_rx,
                    release: release_tx,
                },
            )
        }
    }

    impl ActivationHandle {
        /// Wait, bounded, until an activation has entered its blocked window.
        fn wait_until_activation_started(&self) {
            self.entered
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the insertion never reached its focus activation");
        }

        /// Let the blocked activation finish.
        fn release(&self) {
            let _ = self.release.send(());
        }
    }

    impl TextTargetFocus for BlockingActivation {
        fn frontmost_app(&self) -> Option<i32> {
            self.focus.frontmost_app()
        }

        fn activate(&self, pid: i32) -> bool {
            // Focus has drifted and the real OS would bring the target back; the
            // test only needs the blocking, not the restoration.
            *self.focus.frontmost.lock().unwrap() = Some(pid);
            let _ = self.entered.send(());
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(ACTIVATION_DEADLINE);
            self.focus.activate(pid)
        }
    }

    #[test]
    fn a_cancel_and_a_newer_start_during_a_blocked_focus_restore_still_type_nothing() {
        // The production interleaving with the real runtime: the session is live
        // when focus restoration starts, the target app blocks while it is
        // activated, and the user's Cancel and a newer Start both land in that
        // window. The words must not land afterwards, and neither lifecycle
        // event may wait behind the blocked activation — which is what holding
        // the session guard across the operating-system focus work would have
        // caused (slugtale-cbxb).
        let runtime = Arc::new(
            crate::DictationRuntime::start_with_test_pause(
                Arc::new(UnscopedHost),
                Arc::new(|| 0),
                std::time::Duration::from_millis(30),
            )
            .expect("test runtime starts"),
        );
        let session = runtime.begin();
        let (focus, activation) = BlockingActivation::new();
        let typed = Arc::new(Mutex::new(Vec::new()));

        let inserting = {
            let focus = Arc::clone(&focus) as Arc<dyn TextTargetFocus>;
            let typed = Arc::clone(&typed);
            let runtime = runtime.session_effects();
            std::thread::spawn(move || {
                let mut prepared = PreparedInsertion::new(
                    SettledTextInsertion::targeted(
                        Box::new(RecordingInsertion(typed)),
                        None,
                        Some(7),
                        focus,
                        None,
                    ),
                    Box::new(UnreachableRescue),
                );
                prepared.guard_with_session(session, runtime);
                prepared
                    .insertion
                    .insert(&crate::FinalTranscription::plain("words after cancel".to_string()))
                    .expect("a declined insertion is not a failure");
                prepared.insertion_was_refused()
            })
        };

        activation.wait_until_activation_started();

        // The user's Cancel and the next Start, while the activation is still
        // blocked. Both must return without waiting for the operating system.
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let lifecycling = {
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || {
                runtime.abandon();
                let _ = cancelled_tx.send(());
                let newer = runtime.begin();
                let _ = started_tx.send(newer);
            })
        };
        cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("Cancel must not wait behind the blocked activation");
        let newer = started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a newer Start must not wait behind the blocked activation");
        assert!(
            runtime.is_session_live(newer),
            "the replacement dictation owns the session"
        );
        assert!(!runtime.is_session_live(session));

        activation.release();
        let refused = inserting.join().expect("the insertion thread finishes");
        lifecycling
            .join()
            .expect("the lifecycle thread finishes");

        assert!(
            typed.lock().unwrap().is_empty(),
            "the words must not land after the dictation was retired"
        );
        assert!(refused, "the boundary must report the refusal");
    }

    #[test]
    fn a_cancelled_session_never_reaches_the_clipboard_through_the_rescue() {
        // Insertion failed, and the Cancel lands between the failure and the
        // rescue. The rescue is an effect too, so it is refused on its own account
        // and reports that it preserved nothing.
        let effects = FakeEffects::answering(&[true, true, false]);
        let rescued = Arc::new(Mutex::new(Vec::new()));
        let prepared = scoped(
            Box::new(FailingInsertion),
            Box::new(RecordingRescue(Arc::clone(&rescued))),
            FakeFocus::holding(7),
            Some(7),
            Arc::clone(&effects) as Arc<dyn SessionEffects>,
        );

        let error = prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("words".to_string()))
            .expect_err("the fake insertion fails");

        assert_eq!(
            prepared
                .rescue
                .rescue(&crate::FinalTranscription::plain("words".to_string()))
                .expect_err("a refused rescue preserved nothing")
                .to_string(),
            "insertion rescue failed: the dictation was cancelled before its transcription could be preserved"
        );
        assert!(
            rescued.lock().unwrap().is_empty(),
            "a cancelled dictation must not change the user's clipboard"
        );
        assert!(
            effects.asked() >= 3,
            "the refusal came from a third decision — the rescue's own — not from the keystrokes"
        );
        assert!(prepared.insertion_was_refused());
        assert!(error.to_string().contains("fake insertion failure"));
    }

    #[test]
    fn a_live_session_still_rescues_a_failed_insertion_onto_the_clipboard() {
        // The other half of the rule, so the refusal above cannot be met simply
        // by never rescuing: a live dictation keeps ADR-0016's rescue intact.
        let rescued = Arc::new(Mutex::new(Vec::new()));
        let prepared = scoped(
            Box::new(FailingInsertion),
            Box::new(RecordingRescue(Arc::clone(&rescued))),
            FakeFocus::holding(7),
            Some(7),
            FakeEffects::answering(&[true, true, true]),
        );

        prepared
            .insertion
            .insert(&crate::FinalTranscription::plain("kept words".to_string()))
            .expect_err("the fake insertion fails");
        prepared
            .rescue
            .rescue(&crate::FinalTranscription::plain("kept words".to_string()))
            .expect("a live session's rescue runs");

        assert_eq!(*rescued.lock().unwrap(), ["kept words"]);
        assert!(!prepared.insertion_was_refused());
    }

    #[test]
    fn a_live_session_passes_the_gate_and_types_into_its_confirmed_target() {
        let focus = FakeFocus::holding(7);
        let typed = Arc::new(Mutex::new(Vec::new()));
        let prepared = scoped(
            Box::new(RecordingInsertion(Arc::clone(&typed))),
            Box::new(UnreachableRescue),
            Arc::clone(&focus) as Arc<dyn TextTargetFocus>,
            Some(7),
            FakeEffects::answering(&[true, true]),
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
        // The session is live, but the app the words belong to cannot be
        // confirmed. An error here is what makes the Dictation Workflow preserve
        // the words on the clipboard rather than lose them.
        let focus = FakeFocus::refusing(9);
        let prepared = scoped(
            Box::new(UnreachableInsertion),
            Box::new(UnreachableRescue),
            focus,
            Some(7),
            FakeEffects::answering(&[true, true]),
        );

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

    /// An insertion that always fails, as a real one does without Accessibility.
    struct FailingInsertion;

    impl TextInsertion for FailingInsertion {
        fn insert(
            &self,
            _transcription: &crate::FinalTranscription,
        ) -> Result<(), crate::TextInsertionError> {
            Err(crate::TextInsertionError::new("fake insertion failure"))
        }
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
