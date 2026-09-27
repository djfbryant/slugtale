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
    SegmentPauseDetector, SEGMENT_PAUSE,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// Everything the Dictation Runtime asks of the app, and nothing else. One
/// interface, four methods: the Dictation Host implements it in production, and
/// the tests answer it either from one fake that scripts every behaviour below
/// or from a stub that reaches nothing when the test never gets as far as a
/// Dictation Segment. The runtime holds the host shared for the app's whole
/// life and only ever reads through it, so a host keeps whatever locking its
/// own state needs inside itself.
pub trait DictationRuntimeHost: Send + Sync {
    /// Take the pending Pause Flush audio from the capture ring, cutting at
    /// `cut` — the sample watermark the flush was queued with.
    fn take_pause_segment(&self, cut: u64) -> Option<CapturedAudio>;

    /// Transcribe, clean up, insert, and rescue one segment, start to finish.
    /// Errors are reported as strings because they are logged, never surfaced.
    fn complete(
        &self,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, String>;

    /// Take one Counted Segment toward Usage. The runtime has already decided
    /// it counts; where it goes, and never making the Dictation wait for that,
    /// is the host's half of ADR-0025.
    fn record_counted_segment(&self, segment: CountedSegment);

    /// A dictation's final job has settled — inserted, skipped, failed, or even
    /// panicked. Whatever happens, nothing else will end the transcribing
    /// state: the host hides the Dictation Bar here.
    fn last_job_settled(&self);
}

/// One unit of Dictation Segment work, in the order the runtime heard it.
#[derive(Debug)]
enum DictationSegmentJob {
    PauseFlush {
        dictation: u64,
        /// The ring sample position of the last voiced sample when this flush
        /// was queued. The worker drains only through it (plus the capture
        /// module's quiet-tail guard), so queue delay cannot append later
        /// speech or extra silence to the segment (slugtale-g1o.4).
        cut: u64,
    },
    Last {
        dictation: u64,
        audio: CapturedAudio,
    },
}

impl DictationSegmentJob {
    fn dictation(&self) -> u64 {
        match self {
            Self::PauseFlush { dictation, .. } | Self::Last { dictation, .. } => *dictation,
        }
    }

    fn is_last(&self) -> bool {
        matches!(self, Self::Last { .. })
    }
}

/// Shared Dictation Segment state. The Tauri tier owns transport and audio;
/// this decides which queued work is still valid.
#[derive(Default)]
struct DictationSegmentControl {
    dictation: AtomicU64,
    cancelled_through: AtomicU64,
    rescued: AtomicBool,
}

impl DictationSegmentControl {
    fn current(&self) -> u64 {
        self.dictation.load(Ordering::SeqCst)
    }

    fn begin(&self) -> u64 {
        self.rescued.store(false, Ordering::SeqCst);
        self.dictation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn abandon(&self) {
        self.cancelled_through
            .store(self.current(), Ordering::SeqCst);
    }

    fn is_cancelled(&self, dictation: u64) -> bool {
        dictation <= self.cancelled_through.load(Ordering::SeqCst)
    }

    fn is_recording(&self, dictation: u64) -> bool {
        self.current() == dictation && !self.is_cancelled(dictation)
    }

    fn suspend_flushes_for_rescue(&self) {
        self.rescued.store(true, Ordering::SeqCst);
    }

    /// The ADR-0026 rule "Rescue suspends flushes" (guarantee 3), in one place.
    ///
    /// After Insertion Rescue fires, no later Pause Flush may insert, or the
    /// rescue is buried under new text the user has to dig out. Both sides of
    /// the segment queue ask this one question, and both must: the queue is
    /// unbounded, so `on_voice_level` asks before it queues anything, and the
    /// worker asks again before it drains, because a job queued before the
    /// rescue arrived can still reach the drain. The last segment of the
    /// dictation is never held back, so the words the user said are still kept.
    fn rescue_suspends_flushes(&self) -> bool {
        self.rescued.load(Ordering::SeqCst)
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
    /// `begin()` re-arms it, so every dictation starts with a detector that has
    /// heard nothing and therefore cannot flush before the user has said
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
    dictation: u64,
    inserted_any: bool,
}

impl DictationSegmentWorker {
    fn process(
        &mut self,
        job: DictationSegmentJob,
        control: &DictationSegmentControl,
        host: &dyn DictationRuntimeHost,
    ) -> Result<DictationSegmentJobResult, String> {
        let number = job.dictation();
        let last = job.is_last();
        if number != self.dictation {
            self.dictation = number;
            self.inserted_any = false;
        }

        let audio = match job {
            DictationSegmentJob::PauseFlush { dictation, cut } => {
                if control.rescue_suspends_flushes() || !control.is_recording(dictation) {
                    None
                } else {
                    host.take_pause_segment(cut)
                }
            }
            DictationSegmentJob::Last { audio, .. } => {
                (!control.is_cancelled(number)).then_some(audio)
            }
        };

        let Some(audio) = audio else {
            return Ok(DictationSegmentJobResult::Skipped { last });
        };

        let speaking_seconds = if audio.sample_rate_hz > 0 {
            audio.samples.len() as f64 / f64::from(audio.sample_rate_hz)
        } else {
            0.0
        };
        let starts_dictation = !self.inserted_any;
        let position = if starts_dictation {
            DictationSegmentPosition::First
        } else {
            DictationSegmentPosition::Continuation
        };
        let outcome = host.complete(audio, position)?;
        if outcome.inserted {
            host.record_counted_segment(CountedSegment {
                words: count_words(&outcome.transcription.text),
                speaking_seconds,
                starts_dictation,
            });
        }
        self.inserted_any |= outcome.inserted;
        if outcome.rescued {
            control.suspend_flushes_for_rescue();
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
        Self::start_with_pause(host, Arc::new(voice_watermark), SEGMENT_PAUSE)
    }

    fn start_with_pause(
        host: Arc<dyn DictationRuntimeHost>,
        voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
        pause: std::time::Duration,
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
            pause_detector: Mutex::new(SegmentPauseDetector::with_pause(pause)),
            voice_watermark,
        })
    }

    fn current(&self) -> u64 {
        self.control.current()
    }

    /// Open a new dictation and return its number.
    pub fn begin(&self) -> u64 {
        if let Ok(mut detector) = self.pause_detector.lock() {
            detector.rearm();
        }
        self.control.begin()
    }

    /// Abandon the active dictation's un-inserted remainder.
    pub fn abandon(&self) {
        self.control.abandon();
    }

    /// Queue a Pause Flush for the active dictation, cutting the segment at the
    /// sample watermark `cut`. Reports whether the worker accepted it.
    fn send_pause_flush(&self, cut: u64) -> bool {
        self.send(DictationSegmentJob::PauseFlush {
            dictation: self.current(),
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
        let Ok(mut detector) = self.pause_detector.lock() else {
            return;
        };
        if !detector.on_level(level, std::time::Instant::now()) {
            return;
        }
        if self.control.rescue_suspends_flushes() {
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
            dictation: self.current(),
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

    /// A test in another module drives the same trigger at a pause it does not
    /// have to sit out, which is the only reason this exists.
    #[cfg(test)]
    pub(crate) fn start_with_test_pause(
        host: Arc<dyn DictationRuntimeHost>,
        voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
        pause: std::time::Duration,
    ) -> Result<Self, String> {
        Self::start_with_pause(host, voice_watermark, pause)
    }

    /// A runtime with no worker thread, for tests that read the queued jobs.
    #[cfg(test)]
    fn for_testing(
        pause: std::time::Duration,
        voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> (Self, mpsc::Receiver<DictationSegmentJob>) {
        let control = Arc::new(DictationSegmentControl::default());
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                control,
                jobs: Mutex::new(Some(sender)),
                pause_detector: Mutex::new(SegmentPauseDetector::with_pause(pause)),
                voice_watermark,
            },
            receiver,
        )
    }

    /// The suspension set by a rescue, reached without a worker to set it, so a
    /// test can ask the producer side what it does with a flag already raised.
    #[cfg(test)]
    fn suspend_flushes_for_test(&self) {
        self.control.suspend_flushes_for_rescue();
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
fn settle_job(
    worker: &mut DictationSegmentWorker,
    job: DictationSegmentJob,
    control: &DictationSegmentControl,
    host: &dyn DictationRuntimeHost,
) {
    let last = job.is_last();
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
        host.last_job_settled();
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
        Completed(DictationSegmentPosition),
        Recorded(CountedSegment),
        BarHidden,
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

        fn positions(&self) -> Vec<DictationSegmentPosition> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    HostCall::Completed(position) => Some(position),
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
                .filter(|call| **call == HostCall::BarHidden)
                .count()
        }
    }

    impl DictationRuntimeHost for FakeHost {
        fn take_pause_segment(&self, cut: u64) -> Option<CapturedAudio> {
            self.calls.lock().unwrap().push(HostCall::Cut(cut));
            self.audio.lock().unwrap().remove(0)
        }

        fn complete(
            &self,
            _audio: CapturedAudio,
            position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            let outcome = self.outcomes.lock().unwrap().remove(0);
            let completion = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(HostCall::Completed(position));
                calls
                    .iter()
                    .filter(|call| matches!(call, HostCall::Completed(_)))
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

        fn last_job_settled(&self) {
            self.calls.lock().unwrap().push(HostCall::BarHidden);
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
        let runtime = DictationRuntime::start_with_pause(
            Arc::new(host.clone()),
            Arc::new(|| 16_000),
            TEST_PAUSE,
        )
        .expect("the runtime starts");
        runtime.begin();

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
        let runtime =
            DictationRuntime::start_with_pause(Arc::new(host.clone()), Arc::new(|| 0), TEST_PAUSE)
                .expect("the runtime starts");
        runtime.begin();

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
        let runtime = DictationRuntime::start_with_pause(
            Arc::new(host.clone()),
            Arc::new(|| 16_000),
            TEST_PAUSE,
        )
        .expect("the runtime starts");
        runtime.begin();

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
        let (runtime, queued) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));
        let control = Arc::clone(&runtime.control);
        runtime.begin();

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        let in_flight = queued
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("a flush queues while the dictation records");

        runtime.suspend_flushes_for_test();

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
                    dictation: 1,
                    cut: 16_000,
                },
                DictationSegmentJob::PauseFlush {
                    dictation: 1,
                    cut: 32_000,
                },
                DictationSegmentJob::Last {
                    dictation: 1,
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
                    dictation: 1,
                    cut: 0,
                },
                DictationSegmentJob::Last {
                    dictation: 1,
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
                    dictation: 1,
                    cut: 0,
                },
                DictationSegmentJob::Last {
                    dictation: 1,
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
                dictation: 1,
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
                        dictation: first,
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
        let dictation = control.begin();
        control.suspend_flushes_for_rescue();
        let mut worker = DictationSegmentWorker::default();
        let host = FakeHost::answering(
            vec![Some(audio(1, 1))],
            vec![outcome("rescued", true, true)],
        );

        assert_eq!(
            worker
                .process(
                    DictationSegmentJob::PauseFlush { dictation, cut: 0 },
                    &control,
                    &host
                )
                .unwrap(),
            DictationSegmentJobResult::Skipped { last: false }
        );
        worker
            .process(
                DictationSegmentJob::Last {
                    dictation,
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
                        dictation: next,
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
                    dictation: 1,
                    cut: 0,
                },
                DictationSegmentJob::PauseFlush {
                    dictation: 1,
                    cut: 1_000,
                },
                DictationSegmentJob::Last {
                    dictation: 1,
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
        let rescued_dictation = control.begin();
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
                    dictation: rescued_dictation,
                    cut: 0,
                },
                &control,
                &host,
            )
            .unwrap();

        let next_dictation = control.begin();
        worker
            .process(
                DictationSegmentJob::PauseFlush {
                    dictation: next_dictation,
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
                    dictation: 1,
                    cut: 0,
                },
                DictationSegmentJob::PauseFlush {
                    dictation: 1,
                    cut: 500,
                },
                DictationSegmentJob::Last {
                    dictation: 1,
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
                dictation: 1,
                cut: 0,
            },
            &control,
            &host,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                dictation: 1,
                audio: audio(1, 1),
            },
            &control,
            &host,
        );

        assert!(host.positions().is_empty());
        assert!(host.recorded().is_empty());
        assert_eq!(host.bars_hidden(), 1);
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
                dictation: 1,
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
                dictation: 1,
                audio: audio(1, 1),
            },
            &control,
            &panicking,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                dictation: 1,
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
                dictation: 1,
                cut: 0,
            }],
        );

        assert_eq!(host.bars_hidden(), 0);
    }

    #[test]
    fn a_segment_pause_queues_a_flush_cut_at_the_probed_watermark() {
        let (runtime, receiver) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 42_000));
        runtime.begin();

        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);

        match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(DictationSegmentJob::PauseFlush { dictation, cut }) => {
                assert_eq!(dictation, 1);
                assert_eq!(cut, 42_000);
            }
            other => panic!("expected a Pause Flush at the probed watermark, got {other:?}"),
        }
    }

    #[test]
    fn a_dictation_that_opens_with_silence_never_queues_a_flush() {
        let (runtime, receiver) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));
        runtime.begin();

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
        let (used, used_queue) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));
        let (fresh, fresh_queue) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));

        // Dictation one on the used runtime speaks, then stops short of a
        // pause: nothing flushed, so the detector keeps the last word.
        used.begin();
        used.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE / 4);
        used.on_voice_level(0.0);
        assert!(used_queue.try_recv().is_err(), "no pause elapsed yet");

        // Dictation two on the used runtime, and dictation one on the fresh one.
        used.begin();
        fresh.begin();
        for _ in 0..4 {
            used.on_voice_level(0.0);
            fresh.on_voice_level(0.0);
            std::thread::sleep(TEST_PAUSE * 2);
        }
        assert!(
            used_queue.try_recv().is_err(),
            "a dictation that opens with silence must not flush, however long ago the user spoke"
        );
        assert!(fresh_queue.try_recv().is_err());

        for (runtime, queue) in [(&used, &used_queue), (&fresh, &fresh_queue)] {
            runtime.on_voice_level(speaking());
            std::thread::sleep(TEST_PAUSE * 3);
            runtime.on_voice_level(0.0);
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
        let (runtime, receiver) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));

        // Dictation one speaks and pauses: one flush.
        runtime.begin();
        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok());

        // Dictation two begins with silence. The stale detector from dictation
        // one must not flush it — only speech re-arms a pause.
        runtime.begin();
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
}
