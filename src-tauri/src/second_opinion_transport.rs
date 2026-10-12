
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::{AsrError, CapturedAudio, EngineTranscriber, EngineTranscription};

#[derive(Clone, Default)]
pub(crate) struct InFlightGate(Arc<AtomicBool>);

impl InFlightGate {
    /// Take the gate if nobody holds it. `false` means another escalation is
    /// still decoding, and this one must not start a second worker.
    pub(crate) fn try_acquire(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Run `provider` on its own thread and wait at most `budget` for its answer.
///
/// Returns `None` when the gate was already held (another escalation is in
/// flight), when the budget ran out, or when the engine failed — the caller
/// cannot tell those apart and must not need to: every one of them means the
/// primary transcript stands.
pub(crate) fn run_within_budget(
    provider: Arc<dyn EngineTranscriber>,
    audio: &CapturedAudio,
    gate: &InFlightGate,
    budget: Duration,
) -> Option<EngineTranscription> {
    if !gate.try_acquire() {
        return None;
    }

    let audio = audio.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_gate = gate.clone();
    thread::spawn(move || {
        let _guard = GateGuard::new(worker_gate);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider.transcribe(&audio)))
                .unwrap_or_else(|_| {
                    Err(AsrError::Runtime(
                        "second opinion engine panicked".to_string(),
                    ))
                });
        let _ = sender.try_send(result);
    });

    receiver.recv_timeout(budget).ok().and_then(Result::ok)
}

/// Releases the gate when the worker leaves this scope, however it leaves it.
/// The gate is the shared in-flight state, so an early return or a panic must
/// not strand every later segment's escalation.
struct GateGuard {
    gate: InFlightGate,
}

impl GateGuard {
    fn new(gate: InFlightGate) -> Self {
        Self { gate }
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineAvailability, EngineMetadata, FinalTranscription, TranscriptionEngine};
    use std::sync::atomic::AtomicUsize;

    fn audio() -> CapturedAudio {
        CapturedAudio::mono_16khz(vec![0.1; 32_000])
    }

    fn test_metadata() -> EngineMetadata {
        EngineMetadata {
            engine: TranscriptionEngine::Parakeet,
            model_id: "test",
            capability: "test",
            revision: "test",
            approximate_bytes: None,
            source_url: None,
            license: "test",
            license_url: "https://example.test",
            attribution: None,
            modifications: None,
            system_managed: false,
            supported_platforms: "test",
        }
    }

    struct Worker {
        text: String,
        delay: Option<Duration>,
        fails: bool,
        panics: bool,
        calls: Arc<AtomicUsize>,
    }

    impl Worker {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_string(),
                delay: None,
                fails: false,
                panics: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn slow(mut self, delay: Duration) -> Self {
            self.delay = Some(delay);
            self
        }

        fn failing(mut self) -> Self {
            self.fails = true;
            self
        }

        fn panicking(mut self) -> Self {
            self.panics = true;
            self
        }

        fn as_transcriber(worker: &Arc<Worker>) -> Arc<dyn EngineTranscriber> {
            worker.clone() as Arc<dyn EngineTranscriber>
        }

        fn calls(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.calls)
        }
    }

    impl EngineTranscriber for Worker {
        fn engine(&self) -> TranscriptionEngine {
            TranscriptionEngine::Parakeet
        }

        fn metadata(&self) -> EngineMetadata {
            test_metadata()
        }

        fn availability(&self) -> EngineAvailability {
            EngineAvailability::Available
        }

        fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if self.panics {
                panic!("worker panic");
            }
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            if self.fails {
                return Err(AsrError::Runtime("worker failure".to_string()));
            }
            Ok(EngineTranscription::plain(
                TranscriptionEngine::Parakeet,
                FinalTranscription::plain(format!(
                    "{} ({} samples)",
                    self.text,
                    audio.samples.len()
                )),
                Duration::ZERO,
            ))
        }
    }

    #[test]
    fn a_fast_worker_answers_within_the_budget() {
        let worker = Arc::new(Worker::new("the rescue transcript"));

        let answered = run_within_budget(worker.clone(), &audio(), &gate(), Duration::from_secs(5));

        assert_eq!(
            answered.map(|transcription| transcription.text().to_string()),
            Some("the rescue transcript (32000 samples)".to_string())
        );
        assert_eq!(worker.calls().load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_slow_worker_times_out_without_blocking_the_caller() {
        let worker = Arc::new(Worker::new("too late").slow(Duration::from_millis(600)));
        let started = std::time::Instant::now();

        let answered = run_within_budget(worker, &audio(), &gate(), Duration::from_millis(30));

        assert_eq!(answered, None);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the transport must return at the budget, not when the worker ends"
        );
    }

    #[test]
    fn a_failing_worker_reports_no_answer_rather_than_an_error() {
        let answered = run_within_budget(
            Arc::new(Worker::new("never").failing()),
            &audio(),
            &gate(),
            Duration::from_secs(5),
        );

        assert_eq!(answered, None);
    }

    #[test]
    fn a_panicking_worker_is_caught_and_releases_the_gate() {
        let gate = gate();
        let worker = Arc::new(Worker::new("never").panicking());

        assert_eq!(answered_for(&worker, &gate), None);
        wait_for(|| gate.try_acquire(), "the gate to be released");
    }

    #[test]
    fn a_worker_does_not_start_while_another_is_still_running() {
        let gate = gate();
        let running = Arc::new(Worker::new("still going").slow(Duration::from_millis(400)));
        let waiting = Arc::new(Worker::new("never starts"));
        assert!(gate.try_acquire(), "this test holds the gate on purpose");

        let answered =
            run_within_budget(waiting.clone(), &audio(), &gate, Duration::from_millis(50));

        assert_eq!(answered, None);
        assert_eq!(
            waiting.calls().load(Ordering::Acquire),
            0,
            "a second worker must not start behind the first"
        );
        let _ = running;
    }

    #[test]
    fn the_gate_opens_again_once_a_worker_ends() {
        let gate = gate();
        let worker = Arc::new(Worker::new("the answer").slow(Duration::from_millis(120)));

        assert_eq!(answered_for(&worker, &gate), None);
        wait_for(|| gate.try_acquire(), "the gate to reopen");
    }

    fn gate() -> InFlightGate {
        InFlightGate::default()
    }

    fn answered_for(worker: &Arc<Worker>, gate: &InFlightGate) -> Option<String> {
        run_within_budget(
            Worker::as_transcriber(worker),
            &audio(),
            gate,
            Duration::from_millis(20),
        )
        .map(|transcription| transcription.text().to_string())
    }

    fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }
}
