
use crate::{
    count_words, CapturedAudio, CountedSegment, DictationSegmentOutcome, DictationSegmentPosition,
    SegmentPauseDetector,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

pub trait DictationRuntimeHost: Send + Sync {
    fn take_pause_segment(&self, session: u64, cut: u64) -> Option<CapturedAudio>;

    fn complete(
        &self,
        session: u64,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, String>;

    fn record_counted_segment(&self, segment: CountedSegment);

    fn last_job_settled(&self, session: u64);
}

#[derive(Debug)]
enum DictationSegmentJob {
    PauseFlush {
        session: u64,
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

#[derive(Default)]
struct DictationSegmentControl {
    session: AtomicU64,
    cancelled_through: AtomicU64,
    rescued_through: AtomicU64,
    effects: Mutex<()>,
    lifecycle: Mutex<()>,
}

impl DictationSegmentControl {
    fn current(&self) -> u64 {
        self.session.load(Ordering::SeqCst)
    }

    fn changing_the_session<T>(&self, change: impl FnOnce() -> T) -> T {
        let _lifecycle = self.lifecycle.lock();
        let _effects = self.effects.lock();
        change()
    }

    fn while_recording<T>(&self, session: u64, effect: impl FnOnce() -> T) -> Option<T> {
        let _effects = self.effects.lock();
        let recording = self.current() == session && !self.is_cancelled(session);
        recording.then(effect)
    }

    fn begin(&self) -> u64 {
        self.changing_the_session(|| self.session.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn abandon(&self) {
        self.changing_the_session(|| {
            self.cancelled_through
                .store(self.current(), Ordering::SeqCst);
        });
    }

    fn is_cancelled(&self, session: u64) -> bool {
        session <= self.cancelled_through.load(Ordering::SeqCst)
    }

    fn suspend_flushes_for_rescue(&self, session: u64) {
        self.rescued_through.fetch_max(session, Ordering::SeqCst);
    }

    fn rescue_suspends_flushes(&self, session: u64) -> bool {
        session <= self.rescued_through.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DictationSegmentJobResult {
    Skipped { last: bool },
    Completed { inserted: bool, text_chars: usize },
}

pub struct DictationRuntime {
    control: Arc<DictationSegmentControl>,
    jobs: Mutex<Option<mpsc::Sender<DictationSegmentJob>>>,
    pause_detector: Mutex<SegmentPauseDetector>,
    voice_watermark: Arc<dyn Fn() -> u64 + Send + Sync>,
}

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

        let audio = match job {
            DictationSegmentJob::PauseFlush { session, cut } => control
                .while_recording(session, || {
                    if control.rescue_suspends_flushes(session) {
                        None
                    } else {
                        host.take_pause_segment(session, cut)
                    }
                })
                .flatten(),
            DictationSegmentJob::Last { audio, .. } => control.while_recording(session, || audio),
        };

        let Some(audio) = audio else {
            return Ok(DictationSegmentJobResult::Skipped { last });
        };

        let speaking_seconds = crate::captured_audio_duration(&audio).as_secs_f64();
        let starts_dictation = !self.inserted_any;
        let position = if starts_dictation {
            DictationSegmentPosition::First
        } else {
            DictationSegmentPosition::Continuation
        };
        let outcome = host.complete(session, audio, position)?;
        if outcome.inserted {
            let counted = CountedSegment {
                words: count_words(&outcome.transcription.text),
                speaking_seconds,
                starts_dictation,
            };
            control.while_recording(session, || host.record_counted_segment(counted));
        }
        self.inserted_any |= outcome.inserted;
        if outcome.rescued {
            control.while_recording(session, || control.suspend_flushes_for_rescue(session));
        }

        Ok(DictationSegmentJobResult::Completed {
            inserted: outcome.inserted,
            text_chars: outcome.transcription.text.chars().count(),
        })
    }
}

fn placeholder_pause_detector() -> Mutex<SegmentPauseDetector> {
    Mutex::new(SegmentPauseDetector::with_pause(
        std::time::Duration::from_secs(crate::DEFAULT_SEGMENT_PAUSE_SECS as u64),
    ))
}

impl DictationRuntime {
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
            pause_detector: placeholder_pause_detector(),
            voice_watermark: Arc::new(voice_watermark),
        })
    }

    fn current(&self) -> u64 {
        self.control.current()
    }

    pub fn begin(&self, pause_from_pinned_settings: std::time::Duration) -> u64 {
        if let Ok(mut detector) = self.pause_detector.lock() {
            detector.set_pause(pause_from_pinned_settings);
            detector.rearm();
        }
        self.control.begin()
    }

    #[cfg(test)]
    pub(crate) fn armed_pause(&self) -> std::time::Duration {
        self.pause_detector
            .lock()
            .map(|detector| detector.pause())
            .unwrap_or_default()
    }

    pub fn abandon(&self) {
        self.control.abandon();
    }

    fn send_pause_flush(&self, cut: u64) -> bool {
        self.send(DictationSegmentJob::PauseFlush {
            session: self.current(),
            cut,
        })
    }

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
        if self
            .control
            .rescue_suspends_flushes(self.control.current())
        {
            return;
        }
        self.send_pause_flush((self.voice_watermark)());
    }

    pub fn send_last(&self, audio: CapturedAudio) -> bool {
        self.send(DictationSegmentJob::Last {
            session: self.current(),
            audio,
        })
    }

    fn send(&self, job: DictationSegmentJob) -> bool {
        self.jobs
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|sender| sender.send(job).is_ok()))
            .unwrap_or(false)
    }

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
                pause_detector: placeholder_pause_detector(),
                voice_watermark,
            },
            receiver,
        )
    }

    #[cfg(test)]
    fn suspend_flushes_for_test(&self, session: u64) {
        self.control
            .while_recording(session, || self.control.suspend_flushes_for_rescue(session));
    }

    pub fn session_effects(self: &Arc<Self>) -> Arc<dyn crate::SessionEffects> {
        Arc::new(RuntimeSessionEffects {
            runtime: Arc::clone(self),
        })
    }
}

pub struct RuntimeSessionEffects {
    runtime: Arc<DictationRuntime>,
}

impl crate::SessionEffects for RuntimeSessionEffects {
    fn while_session_live(&self, session: u64, effect: &mut dyn FnMut()) -> bool {
        self.runtime
            .control
            .while_recording(session, effect)
            .is_some()
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
        control.while_recording(session, || host.last_job_settled(session));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinalTranscription, TranscriptSegment, VOICE_LEVEL};

    #[derive(Clone, Debug, PartialEq)]
    enum HostCall {
        Cut(u64),
        Completed(u64, DictationSegmentPosition),
        Recorded(CountedSegment),
        BarHidden(u64),
    }

    #[derive(Clone, Default)]
    struct FakeHost {
        calls: Arc<Mutex<Vec<HostCall>>>,
        audio: Arc<Mutex<Vec<Option<CapturedAudio>>>>,
        outcomes: Arc<Mutex<Vec<DictationSegmentOutcome>>>,
        panic_at_completion: usize,
        restart_during_completion: Option<Arc<DictationSegmentControl>>,
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

    fn is_recording(control: &DictationSegmentControl, session: u64) -> bool {
        control.while_recording(session, || ()).is_some()
    }

    fn drive(host: &FakeHost, jobs: Vec<DictationSegmentJob>) {
        let control = DictationSegmentControl::default();
        control.begin();
        let mut worker = DictationSegmentWorker::default();
        for job in jobs {
            settle_job(&mut worker, job, &control, host);
        }
    }

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


    const TEST_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

    fn speaking() -> f32 {
        VOICE_LEVEL + 0.2
    }

    #[test]
    fn a_segment_pause_reaches_the_text_target_in_spoken_order_and_counts_once() {
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
        assert_eq!(host.bars_hidden(), 0);
    }


    #[test]
    fn segments_land_in_spoken_order_however_long_each_decode_takes() {
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
        assert_eq!(
            host.bars_hidden(),
            0,
            "a cancelled dictation's settlement hides nothing"
        );
    }

    #[test]
    fn a_new_dictation_waits_for_a_flush_deciding_what_audio_to_take() {
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
        let control = Arc::new(DictationSegmentControl::default());
        let old = control.begin();

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
            is_recording(&control, control.current()),
            "the newer dictation is the live one once both have finished"
        );
    }

    #[test]
    fn a_rescue_recorded_after_a_newer_start_never_suspends_the_new_dictation() {
        let control = DictationSegmentControl::default();
        let old = control.begin();

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

        assert!(!is_recording(&control, cancelled));
        assert_eq!(
            host.bars_hidden(),
            0,
            "a cancelled dictation's job must not hide any bar"
        );
    }

    #[test]
    fn a_cancel_arriving_during_a_decode_stops_that_segments_usage_count() {
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

        release_tx.send(()).expect("the decode release reaches the worker");
        working.join().expect("the worker finishes");
        cancelling.join().expect("the cancel thread finishes");

        assert!(
            host.recorded.lock().unwrap().is_empty(),
            "a segment the user cancelled must not reach Usage"
        );
        assert!(!is_recording(&control, session));
    }

    #[test]
    fn a_pause_flush_drain_is_told_which_dictations_audio_it_may_take() {
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
        assert!(is_recording(&control, second));
    }

    #[test]
    fn a_rescue_from_a_dictation_the_user_left_does_not_suspend_the_new_one() {
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
    fn the_voice_level_thread_queues_a_flush_without_waiting_on_the_session_locks() {
        let (runtime, receiver) = DictationRuntime::for_testing(Arc::new(|| 42_000));
        let runtime = Arc::new(runtime);
        runtime.begin(TEST_PAUSE);
        let start = std::time::Instant::now();
        runtime.on_voice_level_at(speaking(), start);

        let _lifecycle = runtime.control.lifecycle.lock();
        let _effects = runtime.control.effects.lock();
        let (done_tx, done_rx) = mpsc::channel();
        let level_thread = {
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || {
                runtime.on_voice_level_at(0.0, start + TEST_PAUSE * 3);
                let _ = done_tx.send(());
            })
        };

        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the level thread must not wait for the lifecycle or the effects lock");
        level_thread.join().expect("the level thread finishes");
        assert!(matches!(
            receiver.try_recv(),
            Ok(DictationSegmentJob::PauseFlush { cut: 42_000, .. })
        ));
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

    #[test]
    fn a_second_dictation_behaves_exactly_as_a_first_dictation() {
        let (used, used_queue) = DictationRuntime::for_testing(Arc::new(|| 7));
        let (fresh, fresh_queue) = DictationRuntime::for_testing(Arc::new(|| 7));
        let start = std::time::Instant::now();

        used.begin(TEST_PAUSE);
        used.on_voice_level_at(speaking(), start);
        let before_pause = start + TEST_PAUSE / 4;
        used.on_voice_level_at(0.0, before_pause);
        assert!(before_pause.duration_since(start) < TEST_PAUSE);
        assert!(used_queue.try_recv().is_err(), "no pause elapsed yet");

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

        runtime.begin(TEST_PAUSE);
        runtime.on_voice_level(speaking());
        std::thread::sleep(TEST_PAUSE * 3);
        runtime.on_voice_level(0.0);
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok());

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
        runtime.begin(saved_pause);
        assert_eq!(runtime.armed_pause(), saved_pause);
        let start = std::time::Instant::now();

        runtime.on_voice_level_at(0.0, start);
        let long_silence = start + TEST_PAUSE * 8;
        assert!(long_silence.duration_since(start) > saved_pause);
        runtime.on_voice_level_at(0.0, long_silence);
        let spoken_at = long_silence + TEST_PAUSE;
        runtime.on_voice_level_at(speaking(), spoken_at);
        assert!(receiver.try_recv().is_err());

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

        runtime.begin(saved_pause);
        assert_eq!(runtime.armed_pause(), saved_pause);
    }
}
