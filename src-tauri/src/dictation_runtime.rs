//! The Dictation Runtime (CONTEXT.md): the module that coordinates ordered
//! Dictation Segment execution. It owns the segment channel, the single worker
//! that keeps Final Transcriptions inserting in spoken order, and the handoff
//! of Counted Segments toward Usage (ADR-0025, ADR-0026). Everything that
//! touches the operating system — the microphone ring, text insertion, the
//! Usage File, the Dictation Bar window — sits behind [`DictationRuntimeHost`],
//! so tests drive the whole flush→transcribe→insert→count path against one fake.

use crate::{
    CapturedAudio, CountedSegment, DictationSegmentControl, DictationSegmentExecution,
    DictationSegmentJob, DictationSegmentJobResult, DictationSegmentOutcome,
    DictationSegmentPosition, DictationSegmentWorker, SegmentPauseDetector, SEGMENT_PAUSE,
};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// What the runtime asks of the host app. One adapter, few methods: tests
/// implement all of them once and reuse the fake across every behaviour.
pub trait DictationRuntimeHost {
    /// Take the pending Pause Flush audio from the capture ring, cutting at
    /// `cut` — the sample watermark the flush was queued with.
    fn take_pause_segment(&mut self, cut: u64) -> Option<CapturedAudio>;

    /// Transcribe, clean up, insert, and rescue one segment, start to finish.
    /// Errors are reported as strings because they are logged, never surfaced.
    fn complete(
        &mut self,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, String>;

    /// Hand a Counted Segment toward the Usage File. Never blocks the workflow.
    fn record_counted_segment(&mut self, segment: CountedSegment);

    /// A dictation's final job has settled — inserted, skipped, failed, or even
    /// panicked. Whatever happens, nothing else will end the transcribing
    /// state: the host hides the Dictation Bar here.
    fn last_job_settled(&mut self);
}

/// Ordered Dictation Segment execution behind one small interface: begin,
/// abandon, queue a flush, queue the last segment. The implementation holds
/// the ordering guarantee, the watermark-cut contract, rescue suspension, and
/// panic containment (ADR-0026).
pub struct DictationRuntime {
    control: Arc<DictationSegmentControl>,
    jobs: Mutex<Option<mpsc::Sender<DictationSegmentJob>>>,
    /// The Segment Pause length this runtime arms its detectors with.
    pause: std::time::Duration,
    /// The Segment Pause detector. `begin()` re-arms it, so every dictation
    /// starts with a detector that has heard nothing and therefore cannot
    /// flush before the user has said anything.
    pause_detector: Mutex<SegmentPauseDetector>,
    /// Reads the capture ring's voiced-sample watermark — the microphone half
    /// of the watermark cut (ADR-0026). Probed only at the moment a flush is
    /// due, never per level sample.
    voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl DictationRuntime {
    /// Start the single worker that transcribes and inserts Dictation Segments.
    ///
    /// Segments are decoded one at a time on purpose. Whisper would happily be
    /// asked for two at once, but then a short segment could overtake a long one
    /// and the user's words would land out of order — so the queue is the
    /// ordering guarantee, and the cost is that a slow segment delays the next.
    pub fn start(
        host: impl DictationRuntimeHost + Send + 'static,
        voice_watermark: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::start_with_pause(host, Arc::new(voice_watermark), SEGMENT_PAUSE)
    }

    fn start_with_pause(
        host: impl DictationRuntimeHost + Send + 'static,
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
            pause,
            pause_detector: Mutex::new(SegmentPauseDetector::with_pause(pause)),
            voice_watermark,
        })
    }

    pub fn current(&self) -> u64 {
        self.control.current()
    }

    /// Open a new dictation and return its number.
    pub fn begin(&self) -> u64 {
        if let Ok(mut detector) = self.pause_detector.lock() {
            *detector = SegmentPauseDetector::with_pause(self.pause);
        }
        self.control.begin()
    }

    /// Abandon the active dictation's un-inserted remainder.
    pub fn abandon(&self) {
        self.control.abandon();
    }

    pub fn pause_flushes_suspended(&self) -> bool {
        self.control.pause_flushes_suspended()
    }

    /// Queue a Pause Flush for the active dictation, cutting the segment at the
    /// sample watermark `cut`. Reports whether the worker accepted it.
    pub fn send_pause_flush(&self, cut: u64) -> bool {
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
        request_flush_if_due(
            &mut detector,
            self.pause_flushes_suspended(),
            level,
            || (self.voice_watermark)(),
            |cut| {
                self.send_pause_flush(cut);
            },
        );
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
                pause,
                pause_detector: Mutex::new(SegmentPauseDetector::with_pause(pause)),
                voice_watermark,
            },
            receiver,
        )
    }

    #[cfg(test)]
    fn suspend_for_test(&self) {
        self.control.suspend_pause_flushes();
    }
}

/// Decide whether a voice level ends a Dictation Segment, and queue the flush
/// if so. Shared by [`DictationRuntime::on_voice_level`] and the module's
/// tests, so the trigger rule below is exercised on the path production uses.
///
/// Cut at the last voiced sample the ring knows about, not at whatever has
/// arrived by the time the worker gets here — queue delay must not turn into
/// extra tail audio in the segment (slugtale-g1o.4).
fn request_flush_if_due<W, S>(
    detector: &mut SegmentPauseDetector,
    suspended: bool,
    level: f32,
    watermark: W,
    send: S,
) where
    W: FnOnce() -> u64,
    S: FnOnce(u64),
{
    if !detector.on_level(level, std::time::Instant::now()) {
        return;
    }
    if suspended {
        return;
    }
    send(watermark());
}

fn run_worker<H: DictationRuntimeHost>(
    receiver: mpsc::Receiver<DictationSegmentJob>,
    control: Arc<DictationSegmentControl>,
    mut host: H,
) {
    let mut worker = DictationSegmentWorker::default();
    while let Ok(job) = receiver.recv() {
        settle_job(&mut worker, job, &control, &mut host);
    }
}

/// Transcribe and insert one queued Dictation Segment. Shared by the worker
/// thread and the module's tests, so the behaviours below are exercised on the
/// same path production uses.
fn settle_job<H: DictationRuntimeHost>(
    worker: &mut DictationSegmentWorker,
    job: DictationSegmentJob,
    control: &DictationSegmentControl,
    host: &mut H,
) {
    let last = job.is_last();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut execution = HostExecution { host };
        worker.process(job, control, &mut execution)
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

/// Adapt the host to the segment-execution seam the policy module defines.
struct HostExecution<'a, H> {
    host: &'a mut H,
}

impl<H: DictationRuntimeHost> DictationSegmentExecution for HostExecution<'_, H> {
    type Error = String;

    fn take_pause_segment(&mut self, cut: u64) -> Option<CapturedAudio> {
        self.host.take_pause_segment(cut)
    }

    fn complete(
        &mut self,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, Self::Error> {
        self.host.complete(audio, position)
    }

    fn record(&mut self, segment: CountedSegment) {
        self.host.record_counted_segment(segment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinalTranscription, TranscriptSegment};

    /// One fake implements the whole host interface; each test reads the facts
    /// it cares about off the same shape.
    #[derive(Default)]
    struct FakeHost {
        audio: Vec<Option<CapturedAudio>>,
        outcomes: Vec<DictationSegmentOutcome>,
        positions: Vec<DictationSegmentPosition>,
        recorded: Vec<CountedSegment>,
        bars_hidden: usize,
    }

    impl DictationRuntimeHost for FakeHost {
        fn take_pause_segment(&mut self, _cut: u64) -> Option<CapturedAudio> {
            self.audio.remove(0)
        }

        fn complete(
            &mut self,
            _audio: CapturedAudio,
            position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            self.positions.push(position);
            let outcome = self.outcomes.remove(0);
            if outcome.transcription.text == "PANIC" {
                panic!("decode exploded");
            }
            Ok(outcome)
        }

        fn record_counted_segment(&mut self, segment: CountedSegment) {
            self.recorded.push(segment);
        }

        fn last_job_settled(&mut self) {
            self.bars_hidden += 1;
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
        }
    }

    /// Drive queued jobs through the same settle path the worker thread uses.
    fn drive(host: &mut FakeHost, jobs: Vec<DictationSegmentJob>) {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        for job in jobs {
            settle_job(&mut worker, job, &control, host);
        }
    }

    #[test]
    fn segments_land_in_spoken_order_however_long_each_decode_takes() {
        // Three flushes whose decode costs are unrelated to their speech
        // duration. Order comes from the queue, never from timing, so the
        // first-spoken words take First position and everything after takes
        // Continuation.
        let mut host = FakeHost {
            audio: vec![Some(audio(16_000, 16_000)); 3],
            outcomes: vec![
                outcome("first words", true, false),
                outcome("second words", true, false),
                outcome("third words", true, false),
            ],
            ..Default::default()
        };

        drive(
            &mut host,
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
            host.positions,
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation,
                DictationSegmentPosition::Continuation,
            ]
        );
        assert_eq!(host.recorded.len(), 3);
        assert!(host.recorded[0].starts_dictation);
        assert!(!host.recorded[1].starts_dictation);
    }

    #[test]
    fn a_rescued_segment_suspends_later_flushes_but_still_finishes_the_last() {
        let mut host = FakeHost {
            audio: vec![Some(audio(1, 1)), None],
            outcomes: vec![
                outcome("rescued words", true, true),
                outcome("never reached", true, false),
                outcome("final words", true, false),
            ],
            ..Default::default()
        };

        drive(
            &mut host,
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

        // The rescued flush inserted as First text; the suspended flush took
        // nothing; the last segment still completed — as a Continuation,
        // because the dictation already inserted words before the rescue.
        assert_eq!(
            host.positions,
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        assert_eq!(host.recorded.len(), 2);
    }

    #[test]
    fn usage_counts_only_segments_that_were_inserted_or_rescued() {
        let mut host = FakeHost {
            audio: vec![Some(audio(1, 1)), Some(audio(1, 1)), Some(audio(1, 1))],
            outcomes: vec![
                outcome("", false, false),
                outcome("heard something", true, false),
                outcome("", false, false),
            ],
            ..Default::default()
        };

        drive(
            &mut host,
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

        assert_eq!(host.recorded.len(), 1);
        assert_eq!(host.recorded[0].words, 2);
    }

    #[test]
    fn cancelling_mid_flight_leaves_queued_segments_uninserted() {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut host = FakeHost {
            audio: vec![Some(audio(1, 1))],
            outcomes: vec![outcome("too late", true, false)],
            ..Default::default()
        };

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
            &mut host,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                dictation: 1,
                audio: audio(1, 1),
            },
            &control,
            &mut host,
        );

        assert!(host.positions.is_empty());
        assert!(host.recorded.is_empty());
        assert_eq!(host.bars_hidden, 1);
    }

    #[test]
    fn the_bar_hides_once_per_dictation_whatever_happens_to_the_last_job() {
        // A failing workflow still settles the bar.
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut failing = FakeHost {
            outcomes: vec![outcome("ignored", true, false)],
            ..Default::default()
        };
        failing.audio.push(None); // take_pause_segment finds nothing
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                dictation: 1,
                audio: audio(1, 1),
            },
            &control,
            &mut failing,
        );
        assert_eq!(failing.bars_hidden, 1);

        // ...and a panicking decode must still settle the bar exactly once,
        // and leave the queue alive so the next flush completes normally.
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut panicking = FakeHost {
            outcomes: vec![
                outcome("PANIC", true, false),
                outcome("after the crash", true, false),
            ],
            audio: vec![Some(audio(1, 1))],
            ..Default::default()
        };
        settle_job(
            &mut worker,
            DictationSegmentJob::Last {
                dictation: 1,
                audio: audio(1, 1),
            },
            &control,
            &mut panicking,
        );
        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                dictation: 1,
                cut: 0,
            },
            &control,
            &mut panicking,
        );
        assert_eq!(panicking.bars_hidden, 1);
        // The panicked decode pushed its position before exploding; the next
        // flush completing proves the queue survived.
        assert_eq!(panicking.positions.len(), 2);
    }

    #[test]
    fn a_pause_flush_never_hides_the_bar_while_a_dictation_continues() {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut host = FakeHost {
            audio: vec![Some(audio(1, 1))],
            outcomes: vec![outcome("mid-dictation words", true, false)],
            ..Default::default()
        };

        settle_job(
            &mut worker,
            DictationSegmentJob::PauseFlush {
                dictation: 1,
                cut: 0,
            },
            &control,
            &mut host,
        );

        assert_eq!(host.bars_hidden, 0);
    }

    // ---- the Pause Flush trigger (ADR-0026) ----

    const TEST_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

    use crate::SEGMENT_VOICE_LEVEL;

    fn speaking() -> f32 {
        SEGMENT_VOICE_LEVEL + 0.2
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

    #[test]
    fn rescue_suspension_stops_the_flush_at_its_source() {
        let (runtime, receiver) = DictationRuntime::for_testing(TEST_PAUSE, Arc::new(|| 7));
        runtime.begin();
        runtime.suspend_for_test();
        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);

        assert!(receiver.try_recv().is_err());
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
