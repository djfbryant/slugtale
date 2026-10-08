//! The Dictation Runtime (CONTEXT.md, ADR-0015, ADR-0026): the module that owns
//! everything a Pause Flush has to get right. It keeps Final Transcriptions
//! inserting in spoken order, cuts each flush at the sample watermark the
//! Segment Pause was detected on, holds back later flushes once Insertion Rescue
//! fires, contains a decode panic to the one segment that caused it, and counts
//! every Counted Segment toward Usage.
//!
//! Every operating-system touch — the microphone ring, text insertion, the Usage
//! File, the Dictation Bar — sits behind [`DictationRuntimeHost`], so the whole
//! voice level → cut → order → count → bar path runs in tests against one fake,
//! and the Dictation Host implements the same interface for real.

use crate::{
    count_words, CapturedAudio, CountedSegment, DictationSegmentOutcome, DictationSegmentPosition,
    SegmentPauseDetector,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// Everything the Dictation Runtime asks of the app, and nothing else. One
/// interface, four methods: the Dictation Host implements it in production, and
/// the tests answer it either from one fake that scripts every behaviour below
/// or from a stub that reaches nothing when the test never gets as far as a
/// Dictation Segment. The runtime holds the host shared for the app's whole
/// life and only ever reads through it, so a host keeps whatever locking its
/// own state needs inside itself.
///
/// Every method that reaches the user carries the `session` — the number
/// [`DictationRuntime::begin`] returned for the dictation the job belongs to. A
/// job can still be running when the user cancels that dictation or starts
/// another one, so the effects it would produce have to be told which dictation
/// they belong to rather than assumed to be current (slugtale-cbxb).
pub trait DictationRuntimeHost: Send + Sync {
    /// Take the pending Pause Flush audio from the capture ring, cutting at
    /// `cut` — the sample watermark the flush was queued with.
    ///
    /// `session` is the dictation the flush belongs to, and the host checks it
    /// against its own capture ownership while holding the capture lock. The
    /// worker's own liveness check happens before this call, so a Cancel and a
    /// newer Start can land in between; without the session the old job would
    /// drain whatever the new recording had captured (slugtale-cbxb).
    fn take_pause_segment(&self, session: u64, cut: u64) -> Option<CapturedAudio>;

    /// Transcribe, clean up, insert, and rescue one segment of `session`, start
    /// to finish. Errors are reported as strings because they are logged, never
    /// surfaced.
    fn complete(
        &self,
        session: u64,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, String>;

    /// Take one Counted Segment toward Usage. The runtime has already decided
    /// it counts; where it goes, and never making the Dictation wait for that,
    /// is the host's half of ADR-0025.
    fn record_counted_segment(&self, segment: CountedSegment);

    /// `session`'s final job has settled — inserted, skipped, failed, or even
    /// panicked. Whatever happens, nothing else will end the transcribing
    /// state: the host hides the Dictation Bar here, and only while `session`
    /// is still the dictation that owns the bar.
    fn last_job_settled(&self, session: u64);
}

/// One unit of Dictation Segment work, in the order the runtime heard it. Both
/// variants carry the `session` the work belongs to, so nothing it produces can
/// be attributed to a later dictation.
#[derive(Debug)]
enum DictationSegmentJob {
    PauseFlush {
        session: u64,
        /// The ring sample position of the last voiced sample when this flush
        /// was queued. The worker drains only through it (plus the capture
        /// module's quiet-tail guard), so queue delay cannot append later
        /// speech or extra silence to the segment (slugtale-g1o.4).
        cut: u64,
    },
    Last {
        session: u64,
        audio: CapturedAudio,
    },
}

impl DictationSegmentJob {
    fn session(&self) -> u64 {
        match self {
            Self::PauseFlush { session, .. } | Self::Last { session, .. } => *session,
        }
    }

    fn is_last(&self) -> bool {
        matches!(self, Self::Last { .. })
    }
}

/// Shared Dictation Segment state. The Tauri tier owns transport and audio;
/// this decides which queued work is still valid.
///
/// Two locks, and holding one while taking the other is the whole design here:
///
/// - `lifecycle` belongs to whoever is starting, cancelling or finishing a
///   dictation. It is never held across a decode, a settling sleep, or a focus
///   activation.
/// - `effects` belongs to whoever is about to let a job reach the user: take the
///   microphone cut, type, rescue, or hide the Dictation Bar. `lifecycle` takes
///   it too, but only for as long as it takes to record a new session number or
///   a cancellation.
///
/// Deciding whether a job may still reach the user and then letting it do so is
/// not one operation, so every such rule takes `effects` once and acts while
/// holding it. A Cancel or a newer Start waits on the same lock, so it cannot
/// slip into the gap between the check and the effect it was meant to prevent
/// (slugtale-cbxb). `lifecycle` before `effects` is the only permitted order.
#[derive(Default)]
struct DictationSegmentControl {
    session: AtomicU64,
    cancelled_through: AtomicU64,
    /// The newest session whose Insertion Rescue suspended later Pause Flushes.
    /// Recorded per session rather than as one flag, so a rescue completing
    /// after the user started another dictation cannot suspend that dictation's
    /// flushes, and so a new dictation does not have to clear a flag an old
    /// completion might set again (slugtale-cbxb).
    rescued_through: AtomicU64,
    effects: Mutex<()>,
    lifecycle: Mutex<()>,
}

impl DictationSegmentControl {
    fn current(&self) -> u64 {
        self.session.load(Ordering::SeqCst)
    }

    /// Open the next dictation. Holding `lifecycle` keeps the session number and
    /// the cancellation record consistent with each other, and `effects` keeps a
    /// job from letting an effect through against the session being replaced.
    fn begin(&self) -> u64 {
        let _lifecycle = self.lifecycle.lock();
        let _effects = self.effects.lock();
        self.session.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn abandon(&self) {
        let _lifecycle = self.lifecycle.lock();
        let _effects = self.effects.lock();
        self.cancelled_through
            .store(self.current(), Ordering::SeqCst);
    }

    fn is_cancelled(&self, session: u64) -> bool {
        session <= self.cancelled_through.load(Ordering::SeqCst)
    }

    /// Whether `session` is still the dictation the app is running: neither
    /// cancelled nor replaced by a newer Start.
    ///
    /// Only ever called while holding `effects`, which is what makes a check
    /// followed by an effect one operation rather than two (slugtale-cbxb).
    fn is_recording(&self, session: u64) -> bool {
        self.current() == session && !self.is_cancelled(session)
    }

    /// Record that `session`'s Insertion Rescue must hold back its later Pause
    /// Flushes. Also called while holding `effects`, so it cannot land between a
    /// newer `begin` and that dictation's first flush (slugtale-cbxb).
    fn suspend_flushes_for_rescue(&self, session: u64) {
        self.rescued_through.fetch_max(session, Ordering::SeqCst);
    }

    /// The ADR-0026 rule "Rescue suspends flushes" (guarantee 3), in one place.
    ///
    /// After Insertion Rescue fires, no later Pause Flush of *that dictation* may
    /// insert, or the rescue is buried under new text the user has to dig out.
    /// Both sides of the segment queue ask this one question, and both must: the
    /// queue is unbounded, so `on_voice_level` asks before it queues anything, and
    /// the worker asks again before it drains, because a job queued before the
    /// rescue arrived can still reach the drain. The last segment of the
    /// dictation is never held back, so the words the user said are still kept.
    fn rescue_suspends_flushes(&self, session: u64) -> bool {
        session <= self.rescued_through.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DictationSegmentJobResult {
    Skipped { last: bool },
    Completed { inserted: bool, text_chars: usize },
}

/// Ordered Dictation Segment execution behind one small interface: begin,
/// abandon, queue a flush, queue the last segment. The implementation holds
/// spoken order, the watermark-cut contract, rescue suspension, and panic
/// containment (ADR-0026).
pub struct DictationRuntime {
    control: Arc<DictationSegmentControl>,
    jobs: Mutex<Option<mpsc::Sender<DictationSegmentJob>>>,
    /// The one Segment Pause detector, kept for the app's whole life.
    /// `begin()` re-arms it with the pause the dictation's own pinned Settings
    /// hold, so every dictation starts with a detector that measures that
    /// length and has heard nothing — it cannot flush before the user has said
    /// anything.
    pause_detector: Mutex<SegmentPauseDetector>,
    /// Reads the capture ring's voiced-sample watermark — the microphone half
    /// of the watermark cut (ADR-0026). Probed only at the moment a flush is
    /// due, never per level sample.
    voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
}

/// A single-worker state machine. One worker holds this for the app lifetime,
/// which is what prevents a later Dictation Segment from overtaking an earlier
/// one when transcription takes longer.
#[derive(Default)]
struct DictationSegmentWorker {
    session: u64,
    inserted_any: bool,
}

impl DictationSegmentWorker {
    fn process(
        &mut self,
        job: DictationSegmentJob,
        control: &DictationSegmentControl,
        host: &dyn DictationRuntimeHost,
    ) -> Result<DictationSegmentJobResult, String> {
        let session = job.session();
        let last = job.is_last();
        if session != self.session {
            self.session = session;
            self.inserted_any = false;
        }

        // Each variant decides and acts under `effects`, so a Cancel or a newer
        // Start cannot land between deciding and taking the audio. The flush's
        // drain additionally re-checks inside the capture lock against the
        // session the capture session belongs to, because that lock is the one
        // a concurrent capture start holds (slugtale-cbxb).
        let audio = match job {
            DictationSegmentJob::PauseFlush { session, cut } => {
                let _effects = control.effects.lock();
                if control.rescue_suspends_flushes(session) || !control.is_recording(session) {
                    None
                } else {
                    host.take_pause_segment(session, cut)
                }
            }
            DictationSegmentJob::Last { audio, .. } => {
                let _effects = control.effects.lock();
                control.is_recording(session).then_some(audio)
            }
        };

        let Some(audio) = audio else {
            return Ok(DictationSegmentJobResult::Skipped { last });
        };
        // From here the audio is this job's own and no lock is held: the decode
        // ahead may take seconds, and no lifecycle event may block behind it.

        let speaking_seconds = crate::captured_audio_duration(&audio).as_secs_f64();
        let starts_dictation = !self.inserted_any;
        let position = if starts_dictation {
            DictationSegmentPosition::First
        } else {
            DictationSegmentPosition::Continuation
        };
        let outcome = host.complete(session, audio, position)?;
        // The Usage handoff is an effect like any other: it writes to the user's
        // Daily Usage Records, and words from a dictation the user cancelled must
        // not appear there after Cancel has returned. Deciding and handing over are
        // one operation under `effects`, which the lifecycle also takes
        // (slugtale-cbxb).
        if outcome.inserted {
            let counted = CountedSegment {
                words: count_words(&outcome.transcription.text),
                speaking_seconds,
                starts_dictation,
            };
            let _effects = control.effects.lock();
            if control.is_recording(session) {
                host.record_counted_segment(counted);
            }
        }
        // A segment refused at its own boundary comes back marked as not
        // inserted, so it neither reaches Usage nor takes the dictation's first
        // position away from the segment that does insert.
        self.inserted_any |= outcome.inserted;
        // A rescue raised by a session the user has already moved on from must
        // not wedge the dictation they are in now: the suspension belongs to the
        // dictation that rescued. Recording it under `effects` also means it
        // cannot land between a newer `begin` and that dictation's first flush
        // (slugtale-cbxb).
        if outcome.rescued {
            let _effects = control.effects.lock();
            if control.is_recording(session) {
                control.suspend_flushes_for_rescue(session);
            }
        }

        Ok(DictationSegmentJobResult::Completed {
            inserted: outcome.inserted,
            text_chars: outcome.transcription.text.chars().count(),
        })
    }
}

impl DictationRuntime {
    /// Start the workers that transcribe, insert, and count Dictation Segments.
    ///
    /// Segments are decoded one at a time on purpose. Whisper would happily be
    /// asked for two at once, but then a short segment could overtake a long one
    /// and the user's words would land out of order — so the queue is the
    /// ordering guarantee, and the cost is that a slow segment delays the next.
    pub fn start(
        host: Arc<dyn DictationRuntimeHost>,
        voice_watermark: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let control = Arc::new(DictationSegmentControl::default());
        let (sender, receiver) = mpsc::channel::<DictationSegmentJob>();
        let worker_control = Arc::clone(&control);
        std::thread::Builder::new()
            .name("slugtale-dictation-segments".to_string())
            .spawn(move || run_worker(receiver, worker_control, host))
            .map_err(|error| error.to_string())?;

        Ok(Self {
            control,
            jobs: Mutex::new(Some(sender)),
            // Every dictation re-arms this from its own pinned Settings at
            // `begin`; this initial value only stands in before the first
            // dictation, when no voice level can reach the detector anyway.
            pause_detector: Mutex::new(SegmentPauseDetector::with_pause(
                std::time::Duration::from_secs(crate::DEFAULT_SEGMENT_PAUSE_SECS as u64),
            )),
            voice_watermark: Arc::new(voice_watermark),
        })
    }

    fn current(&self) -> u64 {
        self.control.current()
    }

    /// Open a new dictation and return its session number, arming its detector
    /// with the Segment Pause from the dictation's own pinned Settings. The
    /// caller derives `pause` from the same snapshot it pins for the job, so a
    /// dictation and the length it flushes at cannot come from different points
    /// in time: a change saved while one runs reaches only the next dictation,
    /// whose Start snapshot carries it, and this one keeps the armed pause.
    pub fn begin(&self, pause: std::time::Duration) -> u64 {
        if let Ok(mut detector) = self.pause_detector.lock() {
            detector.set_pause(pause);
            detector.rearm();
        }
        self.control.begin()
    }

    /// The pause the current (or next) dictation measures. Test-only: it is how
    /// a test proves a dictation armed from its own Settings snapshot.
    #[cfg(test)]
    pub(crate) fn armed_pause(&self) -> std::time::Duration {
        self.pause_detector
            .lock()
            .map(|detector| detector.pause())
            .unwrap_or_default()
    }

    /// Abandon the active dictation's un-inserted remainder.
    pub fn abandon(&self) {
        self.control.abandon();
    }

    /// Queue a Pause Flush for the active dictation, cutting the segment at the
    /// sample watermark `cut`. Reports whether the worker accepted it.
    fn send_pause_flush(&self, cut: u64) -> bool {
        self.send(DictationSegmentJob::PauseFlush {
            session: self.current(),
            cut,
        })
    }

    /// Feed the perceptual voice level to the Segment Pause detector and queue
    /// a Pause Flush when one has elapsed (ADR-0026).
    ///
    /// This runs on the recorder's level-emitter thread, so it must never
    /// block: it takes only its own detector lock plus a brief probe of the
    /// capture ring's watermark, and hands the queue a request rather than
    /// touching the audio session.
    pub fn on_voice_level(&self, level: f32) {
        self.on_voice_level_at(level, std::time::Instant::now());
    }

    fn on_voice_level_at(&self, level: f32, at: std::time::Instant) {
        let Ok(mut detector) = self.pause_detector.lock() else {
            return;
        };
        if !detector.on_level(level, at) {
            return;
        }
        // Asked for the dictation a flush would belong to, so a rescue from a
        // session the user has left cannot hold back this one's flushes
        // (slugtale-cbxb). Read without the effects lock: this runs on the
        // recorder's level-emitter thread, and taking that lock here would put
        // the level thread in contention with the lifecycle it feeds.
        if self
            .control
            .rescue_suspends_flushes(self.control.current())
        {
            return;
        }
        // Cut at the last voiced sample the ring knows about, not at whatever has
        // arrived by the time the worker gets here — queue delay must not turn
        // into extra tail audio in the segment (slugtale-g1o.4).
        self.send_pause_flush((self.voice_watermark)());
    }

    /// Queue the active dictation's final captured audio.
    pub fn send_last(&self, audio: CapturedAudio) -> bool {
        self.send(DictationSegmentJob::Last {
            session: self.current(),
            audio,
        })
    }

    /// Queue a job, reporting whether the worker accepted it.
    fn send(&self, job: DictationSegmentJob) -> bool {
        self.jobs
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|sender| sender.send(job).is_ok()))
            .unwrap_or(false)
    }

    /// A runtime with no worker thread, for tests that read the queued jobs.
    #[cfg(test)]
    fn for_testing(
        voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> (Self, mpsc::Receiver<DictationSegmentJob>) {
        let control = Arc::new(DictationSegmentControl::default());
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                control,
                jobs: Mutex::new(Some(sender)),
                pause_detector: Mutex::new(SegmentPauseDetector::with_pause(
                    std::time::Duration::from_secs(crate::DEFAULT_SEGMENT_PAUSE_SECS as u64),
                )),
                voice_watermark,
            },
            receiver,
        )
    }

    /// The suspension set by a rescue, reached without a worker to set it, so a
    /// test can ask the producer side what it does with a flag already raised.
    #[cfg(test)]
    fn suspend_flushes_for_test(&self, session: u64) {
        let _effects = self.control.effects.lock();
        self.control.suspend_flushes_for_rescue(session);
    }

    /// The runtime's own implementation of [`crate::SessionEffects`], so the Dictation
    /// Host can scope a Dictation Segment's effects to a session without either
    /// module depending on the other's internals.
    pub fn session_effects(self: &Arc<Self>) -> Arc<dyn crate::SessionEffects> {
        Arc::new(RuntimeSessionEffects {
            runtime: Arc::clone(self),
        })
    }
}

/// Answers "may this session still reach the user?" in a way a check-then-act
/// gap cannot slip into: the runtime's `effects` lock is held across the
/// decision *and* the effect, and the lifecycle events that invalidate a session
/// wait on that same lock (slugtale-cbxb).
pub struct RuntimeSessionEffects {
    runtime: Arc<DictationRuntime>,
}

impl crate::SessionEffects for RuntimeSessionEffects {
    fn while_session_live(&self, session: u64, effect: &mut dyn FnMut()) -> bool {
        let _effects = self.runtime.control.effects.lock();
        if !self.runtime.control.is_recording(session) {
            return false;
        }
        effect();
        true
    }
}

fn run_worker(
    receiver: mpsc::Receiver<DictationSegmentJob>,
    control: Arc<DictationSegmentControl>,
    host: Arc<dyn DictationRuntimeHost>,
) {
    let mut worker = DictationSegmentWorker::default();
    while let Ok(job) = receiver.recv() {
        settle_job(&mut worker, job, &control, &*host);
    }
}

/// Transcribe and insert one queued Dictation Segment. Every outcome is
/// contained: a failure is logged, a panic is logged, and in both cases the next
/// job still runs and a dictation's last job still settles its Dictation Bar.
///
/// The bar is settled for the session that owns it, on every path — completed,
/// skipped, failed, or panicked. The host decides whether that session may still
/// hide the bar: a cancelled dictation cleared it itself, and a dictation a newer
/// Start replaced must not take the new bar down with it (slugtale-cbxb).
fn settle_job(
    worker: &mut DictationSegmentWorker,
    job: DictationSegmentJob,
    control: &DictationSegmentControl,
    host: &dyn DictationRuntimeHost,
) {
    let last = job.is_last();
    let session = job.session();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        worker.process(job, control, host)
    }));
    match result {
        Ok(Ok(DictationSegmentJobResult::Completed {
            inserted,
            text_chars,
            ..
        })) => {
            if inserted {
                eprintln!("inserted dictation segment: {text_chars} chars");
            } else {
                eprintln!("dictation segment heard nothing; inserted nothing");
            }
        }
        Ok(Ok(DictationSegmentJobResult::Skipped { .. })) => {}
        Ok(Err(error)) => eprintln!("dictation workflow failed: {error}"),
        Err(_) => eprintln!("dictation segment panicked; the queue stays open"),
    }
    if last {
        // Asking whether this session may still hide the Dictation Bar and hiding
        // it are one operation under `effects`: a newer Start that shows a new bar
        // cannot slip between them and lose its bar to an old dictation's
        // completion (slugtale-cbxb).
        let _effects = control.effects.lock();
        if control.is_recording(session) {
            host.last_job_settled(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinalTranscription, TranscriptSegment, VOICE_LEVEL};

    /// Everything the runtime asked of the host, in the order it asked. One
    /// ordered log rather than a Vec per method, so a test can pin the order
    /// between calls as well as what each one carried.
    #[derive(Clone, Debug, PartialEq)]
    enum HostCall {
        Cut(u64),
        Completed(u64, DictationSegmentPosition),
        Recorded(CountedSegment),
        BarHidden(u64),
    }

    /// The one fake: it answers from a script — the audio each cut finds, the
    /// outcome each completion returns, and which completion panics — and writes
    /// every call to a log the synchronous tests and the threaded worker tests
    /// both read.
    #[derive(Clone, Default)]
    struct FakeHost {
        calls: Arc<Mutex<Vec<HostCall>>>,
        audio: Arc<Mutex<Vec<Option<CapturedAudio>>>>,
        outcomes: Arc<Mutex<Vec<DictationSegmentOutcome>>>,
        /// The 1-based completion that panics, so a test picks the point.
        panic_at_completion: usize,
        /// When set, every completion opens the next dictation just before it
        /// answers: the window in which a decode outlives the dictation it
        /// belongs to.
        restart_during_completion: Option<Arc<DictationSegmentControl>>,
        /// Which session each cut was asked for, so a test can prove the drain
        /// boundary was told whose audio it was about to take.
        sessions_cut: Arc<Mutex<Vec<(u64, u64)>>>,
    }

    impl FakeHost {
        fn answering(
            audio: Vec<Option<CapturedAudio>>,
            outcomes: Vec<DictationSegmentOutcome>,
        ) -> Self {
            Self {
                audio: Arc::new(Mutex::new(audio)),
                outcomes: Arc::new(Mutex::new(outcomes)),
                ..Default::default()
            }
        }

        fn panicking_at(mut self, completion: usize) -> Self {
            self.panic_at_completion = completion;
            self
        }

        fn starting_the_next_dictation_during_completion(
            mut self,
            control: Arc<DictationSegmentControl>,
        ) -> Self {
            self.restart_during_completion = Some(control);
            self
        }

        fn calls(&self) -> Vec<HostCall> {
            self.calls.lock().unwrap().clone()
        }

        fn cuts(&self) -> Vec<u64> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    HostCall::Cut(cut) => Some(cut),
                    _ => None,
                })
                .collect()
        }

        fn sessions(&self) -> Vec<u64> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    HostCall::Completed(session, _) => Some(session),
                    _ => None,
                })
                .collect()
        }

        fn positions(&self) -> Vec<DictationSegmentPosition> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    HostCall::Completed(_, position) => Some(position),
                    _ => None,
                })
                .collect()
        }

        fn recorded(&self) -> Vec<CountedSegment> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    HostCall::Recorded(segment) => Some(segment),
                    _ => None,
                })
                .collect()
        }

        fn bars_hidden(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| matches!(call, HostCall::BarHidden(_)))
                .count()
        }

        /// The (session, cut) pair of every drain, in order.
        fn drained(&self) -> Vec<(u64, u64)> {
            self.sessions_cut.lock().unwrap().clone()
        }
    }

    impl DictationRuntimeHost for FakeHost {
        fn take_pause_segment(&self, session: u64, cut: u64) -> Option<CapturedAudio> {
            self.calls.lock().unwrap().push(HostCall::Cut(cut));
            self.sessions_cut
                .lock()
                .unwrap()
                .push((session, cut));
            self.audio.lock().unwrap().remove(0)
        }

        fn complete(
            &self,
            session: u64,
            _audio: CapturedAudio,
            position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            // The user's next Start lands while this decode is still running.
            if let Some(control) = self.restart_during_completion.as_ref() {
                control.begin();
            }
            let outcome = self.outcomes.lock().unwrap().remove(0);
            let completion = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(HostCall::Completed(session, position));
                calls
                    .iter()
                    .filter(|call| matches!(call, HostCall::Completed(_, _)))
                    .count()
            };
            if completion == self.panic_at_completion {
                panic!("decode exploded");
            }
            Ok(outcome)
        }

        fn record_counted_segment(&self, segment: CountedSegment) {
            self.calls.lock().unwrap().push(HostCall::Recorded(segment));
        }

        fn last_job_settled(&self, session: u64) {
            self.calls.lock().unwrap().push(HostCall::BarHidden(session));
        }
    }

    fn audio(samples: usize, sample_rate_hz: u32) -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.0; samples],
            sample_rate_hz,
        }
    }

    fn outcome(text: &str, inserted: bool, rescued: bool) -> DictationSegmentOutcome {
        DictationSegmentOutcome {
            transcription: FinalTranscription {
                text: text.to_string(),
                segments: Vec::<TranscriptSegment>::new(),
            },
            inserted,
            rescued,
            insertion_failure: None,
        }
    }

    /// Drive queued jobs through the same settle path the worker thread uses, in
    /// one fresh dictation.
    fn drive(host: &FakeHost, jobs: Vec<DictationSegmentJob>) {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        for job in jobs {
            settle_job(&mut worker, job, &control, host);
        }
    }

    /// Wait for the worker thread to make `count` calls, so a threaded test does
    /// not depend on how fast the machine is.
    fn wait_for_calls(host: &FakeHost, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while host.calls().len() < count {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker stalled after {:?}",
                host.calls()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Wait for the dictation's last job to settle the Dictation Bar. Jobs are
    /// ordered, so once the bar is hidden nothing is left to do, and a missing
    /// call in the log means the rule stopped it rather than the worker running
    /// late.
    fn wait_for_the_bar(host: &FakeHost) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while host.bars_hidden() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never settled the bar after {:?}",
                host.calls()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    // ---- the Segment Pause trigger and the segment queue (ADR-0026) ----

    const TEST_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

    fn speaking() -> f32 {
        VOICE_LEVEL + 0.2
    }

    #[test]
    fn a_segment_pause_reaches_the_text_target_in_spoken_order_and_counts_once() {
        // The whole Pause Flush path (CONTEXT.md) through one fake: a voice
        // level ends the segment, the flush is cut at the watermark the ring had
        // at that moment, both segments land in spoken order however long each
        // decode takes, each counts toward Usage, and the Dictation Bar hides
        // only after the dictation's last job.
        let host = FakeHost::answering(
            vec![Some(audio(16_000, 16_000)), Some(audio(8_000, 16_000))],
            vec![
                outcome("first words", true, false),
                outcome("second words", true, false),
            ],
        );
        let runtime = DictationRuntime::start(Arc::new(host.clone()), || 16_000)
            .expect("the runtime starts");
        runtime.begin(TEST_PAUSE);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        runtime.send_last(audio(8_000, 16_000));
        wait_for_calls(&host, 6);

        assert_eq!(host.cuts(), [16_000]);
        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        assert_eq!(
            host.recorded(),
            [
                CountedSegment {
                    words: 2,
                    speaking_seconds: 1.0,
                    starts_dictation: true
                },
                CountedSegment {
                    words: 2,
                    speaking_seconds: 0.5,
                    starts_dictation: false
                },
            ]
        );
        assert_eq!(host.bars_hidden(), 1);
    }

    #[test]
    fn a_decode_that_panics_costs_one_segment_and_neither_the_queue_nor_the_bar() {
        // ADR-0026 panic containment, at a chosen point. The panicked segment
        // inserted nothing, so it counts nothing and the next segment is still
        // the first text of the dictation.
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("exploded", true, false),
                outcome("after the crash", true, false),
            ],
        )
        .panicking_at(1);
        let runtime = DictationRuntime::start(Arc::new(host.clone()), || 0)
            .expect("the runtime starts");
        runtime.begin(TEST_PAUSE);

        runtime.send_pause_flush(0);
        runtime.send_last(audio(1, 1));
        wait_for_calls(&host, 5);

        assert_eq!(host.cuts(), [0]);
        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::First
            ]
        );
        assert_eq!(
            host.recorded(),
            [CountedSegment {
                words: 3,
                speaking_seconds: 1.0,
                starts_dictation: true
            }]
        );
        assert_eq!(host.bars_hidden(), 1);
    }

    #[test]
    fn a_rescue_holds_back_the_pause_that_follows_it_and_the_last_still_lands() {
        // ADR-0026 guarantee 3 from a rescue the workflow itself produced, over
        // the real trigger and the real worker. The fake offers audio for the
        // pause that follows the rescue, so the only thing that can stop it is
        // the rule; and the dictation's own last segment is not held back with
        // it, which is the other half of the same guarantee.
        let host = FakeHost::answering(
            vec![Some(audio(16_000, 16_000)), Some(audio(16_000, 16_000))],
            vec![
                outcome("rescued words", true, true),
                outcome("final words", true, false),
                outcome("words after the rescue", true, false),
            ],
        );
        let runtime = DictationRuntime::start(Arc::new(host.clone()), || 16_000)
            .expect("the runtime starts");
        runtime.begin(TEST_PAUSE);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        wait_for_calls(&host, 3);
        // The rescued segment is in the log, so the rule is armed before the
        // pause that has to meet it.
        assert_eq!(host.recorded()[0].words, 2);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        runtime.send_last(audio(16_000, 16_000));
        wait_for_the_bar(&host);

        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        let recorded = host.recorded();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].words, 2);
    }

    #[test]
    fn rescue_suspends_flushes_on_both_sides_of_the_segment_queue() {
        // ADR-0026 guarantee 3, "Rescue suspends flushes", is one rule asked in
        // two places. The producer side stops queueing new Pause Flushes, and the
        // worker side still takes nothing from a flush that was already queued
        // when Insertion Rescue fired — which is why the queue cannot be drained
        // without asking again.
        let (runtime, queued) = DictationRuntime::for_testing(Arc::new(|| 7));
        let control = Arc::clone(&runtime.control);
        runtime.begin(TEST_PAUSE);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        let in_flight = queued
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("a flush queues while the dictation records");

        runtime.suspend_flushes_for_test(1);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        assert!(queued.try_recv().is_err());

        let host = FakeHost::answering(vec![Some(audio(1, 1))], vec![]);
        let mut worker = DictationSegmentWorker::default();
        settle_job(&mut worker, in_flight, &control, &host);
        assert!(host.cuts().is_empty());
        assert!(host.positions().is_empty());
        // A flush is not the dictation's last job, so it never settles the bar.
        assert_eq!(host.bars_hidden(), 0);
    }

    // ---- spoken order, First versus Continuation, and Usage counting ----

    #[test]
    fn segments_land_in_spoken_order_however_long_each_decode_takes() {
        // Three flushes whose decode costs are unrelated to their speech
        // duration. Order comes from the queue, never from timing, so the
        // first-spoken words take First position and everything after takes
        // Continuation.
        let host = FakeHost::answering(
            vec![Some(audio(16_000, 16_000)); 3],
            vec![
                outcome("first words", true, false),
                outcome("second words", true, false),
                outcome("third words", true, false),
            ],
        );

        drive(
            &host,
            vec![
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 16_000,
                },
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 32_000,
                },
                DictationSegmentJob::Last {
                    session: 1,
                    audio: audio(8_000, 16_000),
                },
            ],
        );

        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation,
                DictationSegmentPosition::Continuation,
            ]
        );
        let recorded = host.recorded();
        assert_eq!(recorded.len(), 3);
        assert!(recorded[0].starts_dictation);
        assert!(!recorded[1].starts_dictation);
    }

    #[test]
    fn keeps_segments_ordered_and_makes_later_text_a_continuation() {
        // Two flushes whose decode costs are unrelated to their speech duration.
        // Order comes from the queue, never from timing, so the first-spoken
        // words take First position and everything after takes Continuation.
        let host = FakeHost::answering(
            vec![Some(audio(16_000, 16_000)), Some(audio(8_000, 16_000))],
            vec![
                outcome("first words", true, false),
                outcome("next words", true, false),
            ],
        );

        drive(
            &host,
            vec![
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 0,
                },
                DictationSegmentJob::Last {
                    session: 1,
                    audio: audio(8_000, 16_000),
                },
            ],
        );

        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        let recorded = host.recorded();
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0].starts_dictation);
        assert!(!recorded[1].starts_dictation);
        assert_eq!(recorded[0].speaking_seconds, 1.0);
        assert_eq!(recorded[1].speaking_seconds, 0.5);
    }

    #[test]
    fn a_silent_segment_does_not_consume_the_first_position() {
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![outcome("", false, false), outcome("words", true, false)],
        );

        drive(
            &host,
            vec![
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 0,
                },
                DictationSegmentJob::Last {
                    session: 1,
                    audio: audio(1, 1),
                },
            ],
        );

        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::First
            ]
        );
        let recorded = host.recorded();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].starts_dictation);
    }

    #[test]
    fn a_pause_flush_cuts_at_its_queued_watermark() {
        // The worker must hand the queued cut to capture untouched: the whole
        // point of the watermark is that queue delay cannot move the segment
        // boundary (slugtale-g1o.4).
        let host =
            FakeHost::answering(vec![Some(audio(1, 1))], vec![outcome("words", true, false)]);

        drive(
            &host,
            vec![DictationSegmentJob::PauseFlush {
                session: 1,
                cut: 48_000,
            }],
        );

        assert_eq!(host.cuts(), [48_000]);
    }

    #[test]
    fn a_stale_pause_flush_from_a_finished_dictation_takes_nothing() {
        let control = DictationSegmentControl::default();
        let first = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(vec![None], vec![]);

        // Stop ended the first dictation and Start began the next; a Pause
        // Flush from the old session is stale and must not transcribe.
        let _ = control.begin();
        assert_eq!(
            worker
                .process(
                    DictationSegmentJob::PauseFlush {
                        session: first,
                        cut: 1_000
                    },
                    &control,
                    &host
                )
                .unwrap(),
            DictationSegmentJobResult::Skipped { last: false }
        );
        assert!(host.cuts().is_empty());
    }

    #[test]
    fn cancellation_and_rescue_suppress_pause_flushes_but_not_the_last_segment() {
        let control = DictationSegmentControl::default();
        let session = control.begin();
        control.suspend_flushes_for_rescue(session);
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("rescued", true, true)],
        );

        assert_eq!(
            worker
                .process(
                    DictationSegmentJob::PauseFlush { session, cut: 0 },
                    &control,
                    &host
                )
                .unwrap(),
            DictationSegmentJobResult::Skipped { last: false }
        );
        worker
            .process(
                DictationSegmentJob::Last {
                    session,
                    audio: audio(1, 1),
                },
                &control,
                &host,
            )
            .unwrap();
        assert_eq!(host.positions(), [DictationSegmentPosition::First]);

        let next = control.begin();
        control.abandon();
        assert_eq!(
            worker
                .process(
                    DictationSegmentJob::Last {
                        session: next,
                        audio: audio(1, 1)
                    },
                    &control,
                    &host
                )
                .unwrap(),
            DictationSegmentJobResult::Skipped { last: true }
        );
    }

    #[test]
    fn a_rescued_segment_suspends_later_flushes_but_still_finishes_the_last() {
        // The second flush is offered audio like any other, so the rule is the
        // only thing that can hold it back: the microphone is never cut for a
        // Dictation Segment the user cannot see, and the words the rescue
        // preserved are not buried under the words after them (ADR-0026).
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("rescued words", true, true),
                outcome("final words", true, false),
                outcome("words after the rescue", true, false),
            ],
        );

        drive(
            &host,
            vec![
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 0,
                },
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 1_000,
                },
                DictationSegmentJob::Last {
                    session: 1,
                    audio: audio(1, 1),
                },
            ],
        );

        // The rescued flush inserted as First text and the suspended one took
        // nothing; the last segment still completed, as a Continuation because
        // the dictation had already inserted words before the rescue.
        assert_eq!(host.cuts(), [0]);
        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        let recorded = host.recorded();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].words, 2);
        assert_eq!(recorded[1].words, 2);
    }

    #[test]
    fn a_rescue_suspends_nothing_in_the_next_dictation() {
        // A rescue holds flushes back until the user moves on, and opening the
        // next dictation is how they move on. Without this the first Insertion
        // Rescue of a session would wedge every dictation that follows it.
        let control = DictationSegmentControl::default();
        let rescued_session = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("rescued words", true, true),
                outcome("next words", true, false),
            ],
        );

        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session: rescued_session,
                    cut: 0,
                },
                &control,
                &host,
            )
            .unwrap();

        let next_session = control.begin();
        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session: next_session,
                    cut: 1_000,
                },
                &control,
                &host,
            )
            .unwrap();

        assert_eq!(host.cuts(), [0, 1_000]);
        // A new dictation's first words are First again, whatever the last one
        // did before it stopped.
        assert_eq!(
            host.positions(),
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::First
            ]
        );
        assert_eq!(host.recorded().len(), 2);
    }

    #[test]
    fn usage_counts_only_segments_that_were_inserted_or_rescued() {
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("", false, false),
                outcome("heard something", true, false),
                outcome("", false, false),
            ],
        );

        drive(
            &host,
            vec![
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 0,
                },
                DictationSegmentJob::PauseFlush {
                    session: 1,
                    cut: 500,
                },
                DictationSegmentJob::Last {
                    session: 1,
                    audio: audio(1, 1),
                },
            ],
        );

        let recorded = host.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].words, 2);
    }

    // ---- cancellation, and the Dictation Bar ----

    #[test]
    fn cancelling_mid_flight_leaves_queued_segments_uninserted() {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("too late", true, false)],
        );

        control.abandon();

        // The Cancel event abandons before the worker drains the queue; both
        // the queued flush and the final segment must insert nothing.
        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                session: 1,
                cut: 0,
            },
            &control,
            &host,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                session: 1,
                audio: audio(1, 1),
            },
            &control,
            &host,
        );

        assert!(host.positions().is_empty());
        assert!(host.recorded().is_empty());
        // Nothing may end the transcribing state either. A cancelled dictation
        // cleared its own bar the moment Escape arrived, so its final job hiding
        // the bar would take down whatever is on screen now — a newer dictation's
        // bar (slugtale-cbxb).
        assert_eq!(
            host.bars_hidden(),
            0,
            "a cancelled dictation's settlement hides nothing"
        );
    }

    #[test]
    fn a_new_dictation_waits_for_a_flush_deciding_what_audio_to_take() {
        // The first of the review's races, forced with a bounded wait: the worker is
        // deciding whether its session may still drain the capture ring when a newer
        // Start arrives. Deciding and asking the microphone are one operation under
        // `effects`, and `begin` takes that same lock, so the Start cannot begin a
        // recording while the old flush is still reaching for the ring (slugtale-cbxb).
        let control = Arc::new(DictationSegmentControl::default());
        let old = control.begin();

        let inside = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        struct BlockingDrain {
            inside: Arc<(Mutex<bool>, std::sync::Condvar)>,
            release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        }

        impl DictationRuntimeHost for BlockingDrain {
            fn take_pause_segment(&self, _session: u64, _cut: u64) -> Option<CapturedAudio> {
                let (lock, condvar) = &*self.inside;
                *lock.lock().unwrap() = true;
                condvar.notify_all();
                let (lock, condvar) = &*self.release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = condvar.wait(released).expect("gate mutex");
                }
                Some(audio(1, 1))
            }

            fn complete(
                &self,
                _session: u64,
                _audio: CapturedAudio,
                _position: DictationSegmentPosition,
            ) -> Result<DictationSegmentOutcome, String> {
                Ok(outcome("words", true, false))
            }

            fn record_counted_segment(&self, _segment: CountedSegment) {}

            fn last_job_settled(&self, _session: u64) {}
        }

        let host: Arc<dyn DictationRuntimeHost> = Arc::new(BlockingDrain {
            inside: Arc::clone(&inside),
            release: Arc::clone(&release),
        });
        let draining = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                let mut worker = DictationSegmentWorker::default();
                settle_job(
                    &mut worker,
                    DictationSegmentJob::PauseFlush {
                        session: old,
                        cut: 8_000,
                    },
                    &control,
                    host.as_ref(),
                );
            })
        };

        {
            let (lock, condvar) = &*inside;
            let mut inside = lock.lock().unwrap();
            while !*inside {
                inside = condvar.wait(inside).expect("gate mutex");
            }
        }

        let (running_tx, running_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let starting = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                let _ = running_tx.send(());
                control.begin();
                let _ = finished_tx.send(());
            })
        };
        running_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the Start thread ran");
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "a Start began a new recording while the old flush was still reaching for the capture ring"
        );

        {
            let (lock, condvar) = &*release;
            *lock.lock().unwrap() = true;
            condvar.notify_all();
        }
        draining.join().expect("the flush finishes");
        starting.join().expect("the Start finishes");
    }

    #[test]
    fn a_new_dictation_waits_for_a_settlement_that_is_deciding_whether_to_hide() {
        // The interleaving the review names, forced with a bounded wait: an old
        // dictation's final job is inside its settlement while a newer Start
        // arrives. Deciding to hide and hiding are one operation under `effects`,
        // and `begin` takes that same lock, so the Start cannot overtake the
        // decision and then lose its bar to it (slugtale-cbxb). With the two
        // steps apart, the Start completes while the hide is still pending, and
        // this test fails.
        let control = Arc::new(DictationSegmentControl::default());
        let old = control.begin();

        // Hold the settlement at the point where it has decided to hide.
        let inside = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        struct BlockingSettlement {
            inside: Arc<(Mutex<bool>, std::sync::Condvar)>,
            release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        }

        impl DictationRuntimeHost for BlockingSettlement {
            fn take_pause_segment(&self, _session: u64, _cut: u64) -> Option<CapturedAudio> {
                None
            }

            fn complete(
                &self,
                _session: u64,
                _audio: CapturedAudio,
                _position: DictationSegmentPosition,
            ) -> Result<DictationSegmentOutcome, String> {
                Err("test host never transcribes".to_string())
            }

            fn record_counted_segment(&self, _segment: CountedSegment) {}

            fn last_job_settled(&self, _session: u64) {
                let (lock, condvar) = &*self.inside;
                *lock.lock().unwrap() = true;
                condvar.notify_all();
                let (lock, condvar) = &*self.release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = condvar.wait(released).expect("gate mutex");
                }
            }
        }

        let host: Arc<dyn DictationRuntimeHost> = Arc::new(BlockingSettlement {
            inside: Arc::clone(&inside),
            release: Arc::clone(&release),
        });
        let settling = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                let mut worker = DictationSegmentWorker::default();
                settle_job(
                    &mut worker,
                    DictationSegmentJob::Last {
                        session: old,
                        audio: audio(1, 1),
                    },
                    &control,
                    host.as_ref(),
                );
            })
        };

        // The settlement has decided to hide and is about to.
        {
            let (lock, condvar) = &*inside;
            let mut inside = lock.lock().unwrap();
            while !*inside {
                inside = condvar.wait(inside).expect("gate mutex");
            }
        }

        // A newer Start arrives while the hide is pending.
        let (running_tx, running_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let starting = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                let _ = running_tx.send(());
                control.begin();
                let _ = finished_tx.send(());
            })
        };
        running_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the Start thread ran");

        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "a Start overtook a settlement that had already decided to hide the bar"
        );

        {
            let (lock, condvar) = &*release;
            *lock.lock().unwrap() = true;
            condvar.notify_all();
        }
        settling.join().expect("the settlement finishes");
        starting.join().expect("the Start finishes");
        assert!(
            control.is_recording(control.current()),
            "the newer dictation is the live one once both have finished"
        );
    }

    #[test]
    fn a_rescue_recorded_after_a_newer_start_never_suspends_the_new_dictation() {
        // The review's fourth race: the suspension is recorded against the
        // dictation that rescued, not against "some dictation". Recording it per
        // session makes the order irrelevant — a late old rescue lands after the
        // newer `begin` and suspends only the session it belongs to (slugtale-cbxb).
        let control = DictationSegmentControl::default();
        let old = control.begin();

        // The user starts again, and only then does the old dictation's rescue
        // record its suspension.
        let newer = control.begin();
        control.suspend_flushes_for_rescue(old);

        assert!(
            !control.rescue_suspends_flushes(newer),
            "the new dictation's flushes must not be suspended by the old one's rescue"
        );
        assert!(
            control.rescue_suspends_flushes(old),
            "the rescuing dictation still holds its own flushes back"
        );
    }

    #[test]
    fn a_dictation_the_user_cancelled_does_not_hide_the_bar_from_its_settled_job() {
        // After Cancel nothing else may end the transcribing state: Cancel cleared
        // the bar itself, and an old job's settlement must not touch whatever is
        // on screen now. The decision and the hide are one operation under
        // `effects` (slugtale-cbxb).
        let control = DictationSegmentControl::default();
        let cancelled = control.begin();
        control.abandon();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(vec![Some(audio(1, 1))], vec![]);

        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                session: cancelled,
                audio: audio(1, 1),
            },
            &control,
            &host,
        );

        assert!(!control.is_recording(cancelled));
        assert_eq!(
            host.bars_hidden(),
            0,
            "a cancelled dictation's job must not hide any bar"
        );
    }

    #[test]
    fn a_cancel_arriving_during_a_decode_stops_that_segments_usage_count() {
        // The Usage handoff is an effect: the decode finishing first is not a
        // licence to write words the user cancelled. The decode here blocks until
        // Cancel has already returned, so the only reachable order is the one
        // that must not count: the worker arriving at the handoff afterwards.
        // Deciding and handing over are one operation under `effects`, which
        // `abandon` also takes (slugtale-cbxb).
        struct DecodingHost {
            entered: std::sync::mpsc::Sender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
            recorded: Mutex<Vec<CountedSegment>>,
        }

        const RELEASE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

        impl DictationRuntimeHost for DecodingHost {
            fn take_pause_segment(&self, _session: u64, _cut: u64) -> Option<CapturedAudio> {
                Some(audio(1, 1))
            }

            fn complete(
                &self,
                _session: u64,
                _audio: CapturedAudio,
                _position: DictationSegmentPosition,
            ) -> Result<DictationSegmentOutcome, String> {
                let _ = self.entered.send(());
                // Bounded: a failing test must end this wait, not park the worker.
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(RELEASE_DEADLINE);
                Ok(outcome("words the user cancelled", true, false))
            }

            fn record_counted_segment(&self, segment: CountedSegment) {
                self.recorded.lock().unwrap().push(segment);
            }

            fn last_job_settled(&self, _session: u64) {}
        }

        let control = Arc::new(DictationSegmentControl::default());
        let session = control.begin();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let host = Arc::new(DecodingHost {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            recorded: Mutex::new(Vec::new()),
        });

        let working = {
            let control = Arc::clone(&control);
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                let mut worker = DictationSegmentWorker::default();
                settle_job(
                    &mut worker,
                    DictationSegmentJob::PauseFlush { session, cut: 0 },
                    &control,
                    &*host,
                );
            })
        };

        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the segment reached its decode");

        // Cancel returns while the decode is still blocked: it does not queue
        // behind the model that is thinking.
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let cancelling = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || {
                control.abandon();
                let _ = cancelled_tx.send(());
            })
        };
        cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("Cancel must not wait behind the decode");

        // Let the decode finish and watch the handoff boundary.
        release_tx.send(()).expect("the decode release reaches the worker");
        working.join().expect("the worker finishes");
        cancelling.join().expect("the cancel thread finishes");

        assert!(
            host.recorded.lock().unwrap().is_empty(),
            "a segment the user cancelled must not reach Usage"
        );
        assert!(!control.is_recording(session));
    }

    #[test]
    fn a_pause_flush_drain_is_told_which_dictations_audio_it_may_take() {
        // The worker's liveness check happens before it asks the microphone for
        // anything, so the boundary itself has to know whose audio it is about to
        // take — otherwise a Cancel and a newer Start landing in between would
        // hand an old job the new recording's speech (slugtale-cbxb).
        let control = DictationSegmentControl::default();
        let session = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("words", true, false)],
        );

        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session,
                    cut: 48_000,
                },
                &control,
                &host,
            )
            .unwrap();

        assert_eq!(host.drained(), [(session, 48_000)]);
    }

    #[test]
    fn a_superseded_dictation_drops_every_job_it_still_had_queued() {
        // Cancel a slow dictation and start another one before its final job is
        // drained. Neither the pending flush nor the final segment may reach the
        // host: their words would land in the app the new dictation is aimed at,
        // and the old job's bar hide would take the new bar down (slugtale-cbxb).
        let control = DictationSegmentControl::default();
        let first = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("old words", true, false)],
        );

        control.abandon();
        let second = control.begin();

        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                session: first,
                cut: 1_000,
            },
            &control,
            &host,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                session: first,
                audio: audio(1, 1),
            },
            &control,
            &host,
        );

        assert!(host.cuts().is_empty(), "no segment may be taken for a dead session");
        assert!(host.positions().is_empty());
        assert!(host.recorded().is_empty());
        assert_eq!(
            host.bars_hidden(),
            0,
            "the replaced dictation's settlement must not hide the new bar"
        );
        // ...and the dictation the user is actually running is unaffected.
        assert!(control.is_recording(second));
    }

    #[test]
    fn a_rescue_from_a_dictation_the_user_left_does_not_suspend_the_new_one() {
        // Insertion Rescue suspends later Pause Flushes so the rescued words are
        // not buried. That suspension belongs to the dictation that rescued them,
        // and a decode long enough for the user to start another dictation can
        // finish after they have: the rescue must not wedge the dictation they
        // are in now (slugtale-cbxb). The fake host starts the next dictation
        // while this completion is still in flight, which is exactly that window.
        let control = Arc::new(DictationSegmentControl::default());
        let first = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("rescued words", true, true),
                outcome("next words", true, false),
            ],
        )
        .starting_the_next_dictation_during_completion(Arc::clone(&control));

        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session: first,
                    cut: 0,
                },
                &control,
                &host,
            )
            .unwrap();

        assert!(
            !control.rescue_suspends_flushes(first),
            "the rescue belongs to a dictation the user has already left"
        );

        // And the dictation they are in now flushes normally.
        let second = control.current();
        assert!(second > first);
        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session: second,
                    cut: 1_000,
                },
                &control,
                &host,
            )
            .unwrap();
        assert_eq!(host.cuts(), [0, 1_000]);
    }

    #[test]
    fn a_live_dictation_still_suspends_its_own_flushes_after_a_rescue() {
        // The other half of the same rule, so the fix above cannot simply switch
        // the suspension off: a rescue inside the running dictation still holds
        // back later Pause Flushes (ADR-0026).
        let control = DictationSegmentControl::default();
        let session = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("rescued words", true, true)],
        );

        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session,
                    cut: 0,
                },
                &control,
                &host,
            )
            .unwrap();

        assert!(control.rescue_suspends_flushes(session));
    }

    #[test]
    fn every_job_carries_the_session_its_effects_belong_to() {
        // The host resolves Settings and the text target per session, so the
        // number it is handed has to be the job's own (slugtale-cbxb).
        let control = DictationSegmentControl::default();
        let first = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1)), Some(audio(1, 1))],
            vec![
                outcome("first words", true, false),
                outcome("second words", true, false),
            ],
        );

        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    session: first,
                    cut: 0,
                },
                &control,
                &host,
            )
            .unwrap();

        let second = control.begin();
        worker
            .process(
                DictationSegmentJob::Last {
                    session: second,
                    audio: audio(1, 1),
                },
                &control,
                &host,
            )
            .unwrap();

        assert_eq!(host.sessions(), [first, second]);
    }

    #[test]
    fn the_bar_hides_once_per_dictation_whatever_happens_to_the_last_job() {
        // A failing workflow still settles the bar.
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let empty = FakeHost::answering(vec![None], vec![outcome("ignored", true, false)]);
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                session: 1,
                audio: audio(1, 1),
            },
            &control,
            &empty,
        );
        assert_eq!(empty.bars_hidden(), 1);

        // ...and a panicking decode must still settle the bar exactly once,
        // and leave the queue alive so the next flush completes normally.
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let panicking = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![
                outcome("exploded", true, false),
                outcome("after the crash", true, false),
            ],
        )
        .panicking_at(1);
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                session: 1,
                audio: audio(1, 1),
            },
            &control,
            &panicking,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                session: 1,
                cut: 0,
            },
            &control,
            &panicking,
        );
        assert_eq!(panicking.bars_hidden(), 1);
        // The panicked decode pushed its position before exploding; the next
        // flush completing proves the queue survived.
        assert_eq!(panicking.positions().len(), 2);
    }

    #[test]
    fn a_pause_flush_never_hides_the_bar_while_a_dictation_continues() {
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("mid-dictation words", true, false)],
        );

        drive(
            &host,
            vec![DictationSegmentJob::PauseFlush {
                session: 1,
                cut: 0,
            }],
        );

        assert_eq!(host.bars_hidden(), 0);
    }

    #[test]
    fn a_segment_pause_queues_a_flush_cut_at_the_probed_watermark() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 42_000));
        runtime.begin(TEST_PAUSE);

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);

        match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(DictationSegmentJob::PauseFlush { session, cut }) => {
                assert_eq!(session, 1);
                assert_eq!(cut, 42_000);
            }
            other => panic!("expected a Pause Flush at the probed watermark, got {other:?}"),
        }
    }

    #[test]
    fn a_dictation_that_opens_with_silence_never_queues_a_flush() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 7));
        runtime.begin(TEST_PAUSE);

        for _ in 0..5 {
            runtime.on_voice_level(0.0);
            std::thread::sleep(TEST_PAUSE * 2);
        }

        assert!(receiver.try_recv().is_err());
    }

    /// `begin()` re-arms the one detector the runtime keeps rather than
    /// building a new one, so a second dictation has to behave exactly as a
    /// first one does. The case that bites is a first dictation that ended
    /// before it flushed: the detector still remembers the last word, and a
    /// second dictation that opens with silence must not inherit it. Silence
    /// long enough for a Segment Pause, then speech, then silence again, so the
    /// test also covers the dictation that does flush once spoken to.
    #[test]
    fn a_second_dictation_behaves_exactly_as_a_first_dictation() {
        let (used, used_queue) = DictationRuntime::for_testing(Arc::new(|| 7));
        let (fresh, fresh_queue) = DictationRuntime::for_testing(Arc::new(|| 7));
        let start = std::time::Instant::now();

        // Dictation one on the used runtime speaks, then stops short of a
        // pause: nothing flushed, so the detector keeps the last word.
        used.begin(TEST_PAUSE);
        used.on_voice_level_at(speaking(), start);
        let before_pause = start + TEST_PAUSE / 4;
        used.on_voice_level_at(0.0, before_pause);
        assert!(before_pause.duration_since(start) < TEST_PAUSE);
        assert!(used_queue.try_recv().is_err(), "no pause elapsed yet");

        // Dictation two on the used runtime, and dictation one on the fresh one.
        used.begin(TEST_PAUSE);
        fresh.begin(TEST_PAUSE);
        let mut at = start + TEST_PAUSE * 2;
        for _ in 0..4 {
            used.on_voice_level_at(0.0, at);
            fresh.on_voice_level_at(0.0, at);
            at += TEST_PAUSE * 2;
        }
        assert!(at.duration_since(start) > TEST_PAUSE);
        assert!(
            used_queue.try_recv().is_err(),
            "a dictation that opens with silence must not flush, however long ago the user spoke"
        );
        assert!(fresh_queue.try_recv().is_err());

        for (runtime, queue) in [(&used, &used_queue), (&fresh, &fresh_queue)] {
            runtime.on_voice_level_at(speaking(), at);
            let quiet_at = at + TEST_PAUSE * 3;
            assert!(quiet_at.duration_since(at) >= TEST_PAUSE);
            runtime.on_voice_level_at(0.0, quiet_at);
            assert!(
                queue
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .is_ok(),
                "both dictations flush once the user speaks"
            );
        }
    }

    #[test]
    fn begin_rearms_the_detector_so_each_dictation_needs_fresh_speech() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 7));

        // Dictation one speaks and pauses: one flush.
        runtime.begin(TEST_PAUSE);
        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok());

        // Dictation two begins with silence. The stale detector from dictation
        // one must not flush it — only speech re-arms a pause.
        runtime.begin(TEST_PAUSE);
        for _ in 0..4 {
            runtime.on_voice_level(0.0);
            std::thread::sleep(TEST_PAUSE * 2);
        }
        assert!(receiver.try_recv().is_err());

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok());
    }

    #[test]
    fn a_pause_from_the_start_snapshot_arms_the_dictation() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 7));
        let saved_pause = TEST_PAUSE * 6;
        // The length comes from the caller's Start snapshot; `begin` arms from
        // this argument, so there is no separate moment at which a save could
        // publish a stale length to a dictation about to start.
        runtime.begin(saved_pause);
        assert_eq!(runtime.armed_pause(), saved_pause);
        let start = std::time::Instant::now();

        // Opening silence longer than the new pause never flushes.
        runtime.on_voice_level_at(0.0, start);
        let long_silence = start + TEST_PAUSE * 8;
        assert!(long_silence.duration_since(start) > saved_pause);
        runtime.on_voice_level_at(0.0, long_silence);
        let spoken_at = long_silence + TEST_PAUSE;
        runtime.on_voice_level_at(speaking(), spoken_at);
        assert!(receiver.try_recv().is_err());

        // Quiet for less than the new pause is not enough; the old pause would
        // already have flushed here.
        let before_saved_pause = spoken_at + saved_pause - std::time::Duration::from_millis(1);
        assert!(before_saved_pause.duration_since(spoken_at) < saved_pause);
        runtime.on_voice_level_at(0.0, before_saved_pause);
        assert!(
            receiver.try_recv().is_err(),
            "the new pause has not elapsed yet"
        );

        let saved_pause_elapsed = spoken_at + saved_pause;
        assert!(saved_pause_elapsed.duration_since(spoken_at) >= saved_pause);
        runtime.on_voice_level_at(0.0, saved_pause_elapsed);
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(DictationSegmentJob::PauseFlush { .. })
        ));
    }

    #[test]
    fn a_later_pause_choice_reaches_only_the_next_dictation() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 7));
        let first_pause = TEST_PAUSE;
        runtime.begin(first_pause);

        // The dictation in progress runs at the pause its own Start snapshot
        // named; a choice saved while it runs is not part of that begin, so
        // quiet for the armed pause ends it even though settings now ask for
        // longer.
        let saved_pause = TEST_PAUSE * 6;
        assert_eq!(runtime.armed_pause(), first_pause);
        let spoken_at = std::time::Instant::now();
        runtime.on_voice_level_at(speaking(), spoken_at);

        let just_before_pause = spoken_at + first_pause - std::time::Duration::from_millis(1);
        assert!(just_before_pause.duration_since(spoken_at) < first_pause);
        runtime.on_voice_level_at(0.0, just_before_pause);
        assert!(
            receiver.try_recv().is_err(),
            "the armed pause has not elapsed"
        );

        let armed_pause_elapsed = spoken_at + first_pause;
        assert!(armed_pause_elapsed.duration_since(spoken_at) >= first_pause);
        runtime.on_voice_level_at(0.0, armed_pause_elapsed);
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(DictationSegmentJob::PauseFlush { .. })
        ));

        // The next dictation's own Start snapshot names the newly saved pause.
        runtime.begin(saved_pause);
        assert_eq!(runtime.armed_pause(), saved_pause);
    }
}
