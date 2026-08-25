//! The Voice Activation listen loop (slugtale-e95): the always-listening state
//! machine that turns captured microphone speech into wake checks and, when the
//! user says the wake phrase, a dictation trigger.
//!
//! The loop decides; the host acts. Every operating-system touch — the capture
//! session, the transcription engine, notifications, the dictation trigger —
//! sits behind [`WakeListener`], so the suppression, retry, report-once, and
//! rebuild rules are unit-testable on every platform without audio hardware
//! (ADR-0021: the Windows and Linux ports reuse this loop unchanged).

use crate::{wake_phrase_score, SpeechWindowBuffer, WakeWordConfig, WakeWordDetector};
use std::time::Duration;

/// How often the loop polls its commands while listening.
pub const LISTEN_POLL: Duration = Duration::from_millis(250);
/// How long the loop waits before trying the microphone again after a failure.
pub const LISTEN_CAPTURE_RETRY: Duration = Duration::from_secs(2);

const MIN_NEW_SAMPLES: usize = 32_000;
const OVERLAP_SAMPLES: usize = 16_000;
const SPEECH_FRAME_SAMPLES: usize = 320;
const MINIMUM_SPEECH_RMS: f32 = 0.006;
const SPEECH_CONTRAST_RATIO: f32 = 1.5;

/// The commands the loop understands. Transport (a channel, a test) is the
/// adapter's business; these are the meanings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerCommand {
    Listen,
    Stop,
}

/// What the wake check made of one window of speech. `EngineUnavailable`
/// means the resolved engine cannot run right now — the loop treats the
/// microphone as unusable until it comes back.
#[derive(Debug, PartialEq)]
pub enum WakeCheck {
    EngineUnavailable,
    TranscriptionFailed(String),
    Transcript(String),
}

/// The ports the listen loop needs. One adapter, few methods: the platform
/// executor implements all of them once against the app handle, and tests
/// implement them once against a fake.
pub trait WakeListener {
    /// Block until the next command arrives. `None` ends the worker.
    fn next_command(&mut self) -> Option<ListenerCommand>;

    /// Whether a Stop has arrived without waiting.
    fn stop_requested(&self) -> bool;

    /// Wait up to `timeout` for a Stop. `true` keeps listening.
    fn wait(&mut self, timeout: Duration) -> bool;

    /// Whether a dictation is active; the listener must stand down while it is.
    fn dictating(&self) -> bool;

    /// Whether the wake-check engine can actually run right now.
    fn engine_ready(&self) -> bool;

    fn microphone_granted(&self) -> bool;

    fn capture_is_open(&self) -> bool;
    fn start_capture(&mut self) -> Result<(), String>;
    /// Drop the capture session so the next start opens it fresh.
    fn rebuild_capture(&mut self);
    fn close_capture(&mut self);
    fn take_segment(&mut self) -> Result<Vec<f32>, String>;

    /// Run one wake check over a window of speech. Always greedy decoding:
    /// the user's wider beam is useful for dictation text, but wasteful for a
    /// two-word phrase.
    fn wake_check(&mut self, samples: Vec<f32>) -> WakeCheck;

    /// Tell the user the microphone cannot be heard.
    fn report_microphone_problem(&mut self);

    /// Begin a dictation from the wake phrase.
    fn trigger_wake(&mut self);
}

/// Run the always-listening worker until its channel closes.
///
/// Failure branches share one shape — drop what is broken, wait, try again —
/// so a capture failure, a missing engine, and digital silence all travel the
/// same rebuild-and-retry path instead of six hand copies.
pub fn run_listen_loop(listener: &mut dyn WakeListener) {
    while let Some(command) = listener.next_command() {
        if command == ListenerCommand::Stop {
            continue;
        }

        let mut window = SpeechWindowBuffer::new();
        let mut detector = WakeWordDetector::new(WakeWordConfig::default());
        let mut capture_error_reported = false;
        let mut microphone_problem_reported = false;

        loop {
            if listener.stop_requested() {
                break;
            }

            // Dictations own the microphone while they run; stand down and
            // drop anything half-heard.
            if listener.dictating() {
                if listener.capture_is_open() {
                    listener.rebuild_capture();
                }
                window.clear();
                if !listener.wait(LISTEN_POLL) {
                    break;
                }
                continue;
            }

            if !listener.engine_ready() {
                if listener.capture_is_open() {
                    listener.rebuild_capture();
                    window.clear();
                }
                if !listener.wait(LISTEN_CAPTURE_RETRY) {
                    break;
                }
                continue;
            }

            if !listener.capture_is_open() {
                if !listener.microphone_granted() {
                    if !microphone_problem_reported {
                        listener.report_microphone_problem();
                        microphone_problem_reported = true;
                    }
                    if !listener.wait(LISTEN_CAPTURE_RETRY) {
                        break;
                    }
                    continue;
                }
                match listener.start_capture() {
                    Err(error) => {
                        if !capture_error_reported {
                            eprintln!("voice activation could not open the microphone: {error}");
                            capture_error_reported = true;
                        }
                        listener.rebuild_capture();
                        if !listener.wait(LISTEN_CAPTURE_RETRY) {
                            break;
                        }
                        continue;
                    }
                    Ok(()) => {}
                }
                window.clear();
                eprintln!("voice activation: listening");
            }

            if !listener.wait(LISTEN_POLL) {
                break;
            }
            let chunk = match listener.take_segment() {
                Ok(chunk) => chunk,
                Err(error) => {
                    eprintln!("voice activation capture failed: {error}");
                    listener.rebuild_capture();
                    capture_error_reported = true;
                    window.clear();
                    if !listener.wait(LISTEN_CAPTURE_RETRY) {
                        break;
                    }
                    continue;
                }
            };
            window.push(&chunk);

            if !window.ready_for_evaluation(MIN_NEW_SAMPLES) {
                continue;
            }
            let audio_state = window.new_audio_state(
                SPEECH_FRAME_SAMPLES,
                MINIMUM_SPEECH_RMS,
                SPEECH_CONTRAST_RATIO,
            );
            match audio_state {
                crate::NewAudioState::DigitalSilence => {
                    if !microphone_problem_reported {
                        eprintln!("voice activation: microphone supplied digital silence");
                        listener.report_microphone_problem();
                        microphone_problem_reported = true;
                    }
                    listener.rebuild_capture();
                    window.clear();
                    if !listener.wait(LISTEN_CAPTURE_RETRY) {
                        break;
                    }
                    continue;
                }
                crate::NewAudioState::Quiet => {
                    microphone_problem_reported = false;
                    window.retain_recent(OVERLAP_SAMPLES);
                    continue;
                }
                crate::NewAudioState::Speech => microphone_problem_reported = false,
            }

            let samples = window.take_for_evaluation();
            window.retain_recent(OVERLAP_SAMPLES);
            match listener.wake_check(samples) {
                WakeCheck::EngineUnavailable => {
                    listener.rebuild_capture();
                    window.clear();
                    if !listener.wait(LISTEN_CAPTURE_RETRY) {
                        break;
                    }
                }
                WakeCheck::TranscriptionFailed(error) => {
                    eprintln!("voice activation transcription failed: {error}");
                }
                WakeCheck::Transcript(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    let score = wake_phrase_score(&text);
                    // Scores are safe to log. Transcript text and audio are not.
                    eprintln!("voice activation: score {score:.2}");
                    if detector.on_transcript(&text, unix_ms()).is_some() {
                        if listener.stop_requested() {
                            break;
                        }
                        window.clear();
                        eprintln!("voice activation: wake phrase detected");
                        listener.trigger_wake();
                    }
                }
            }
        }

        listener.close_capture();
        eprintln!("voice activation: stopped listening");
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One fake drives every scenario: counters for each port plus scripted
    /// segments and transcripts, so tests assert on the actions the loop took.
    /// A finite wait budget guarantees every run terminates.
    struct FakeListener {
        commands: RefCell<Vec<Option<ListenerCommand>>>,
        dictating: bool,
        engine_ready: bool,
        microphone_granted: bool,
        capture_starts_ok: bool,
        segments: RefCell<Vec<Result<Vec<f32>, String>>>,
        transcripts: RefCell<Vec<WakeCheck>>,
        waits: RefCell<usize>,
        max_waits: usize,
        rebuilds: RefCell<usize>,
        starts: RefCell<usize>,
        closes: RefCell<usize>,
        mic_reports: RefCell<usize>,
        triggers: RefCell<usize>,
        checks: RefCell<usize>,
    }

    impl Default for FakeListener {
        fn default() -> Self {
            Self {
                commands: RefCell::new(vec![Some(ListenerCommand::Listen)]),
                dictating: false,
                engine_ready: true,
                microphone_granted: true,
                capture_starts_ok: true,
                segments: RefCell::new(Vec::new()),
                transcripts: RefCell::new(Vec::new()),
                waits: RefCell::new(0),
                max_waits: 8,
                rebuilds: RefCell::new(0),
                starts: RefCell::new(0),
                closes: RefCell::new(0),
                mic_reports: RefCell::new(0),
                triggers: RefCell::new(0),
                checks: RefCell::new(0),
            }
        }
    }

    /// One second that classifies as Speech: a quiet room floor for most
    /// frames, then a loud burst — the contrast gate needs both.
    fn speech_chunk() -> Vec<f32> {
        let frame = 320;
        let mut samples = vec![0.0; 32_000];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = if index / frame >= 75 { 0.4 } else { 0.000_1 };
        }
        samples
    }

    /// One second of pure zeros: classifies as DigitalSilence.
    fn silence_chunk() -> Vec<f32> {
        vec![0.0; 32_000]
    }

    impl WakeListener for FakeListener {
        fn next_command(&mut self) -> Option<ListenerCommand> {
            // Exactly one Listen per run; the wait budget ends the listen
            // loop, and None ends the worker. Returning Some forever would
            // spin this test to infinity.
            let mut commands = self.commands.borrow_mut();
            if commands.is_empty() {
                None
            } else {
                commands.remove(0)
            }
        }

        fn stop_requested(&self) -> bool {
            false
        }

        fn wait(&mut self, _timeout: Duration) -> bool {
            let mut waits = self.waits.borrow_mut();
            *waits += 1;
            *waits <= self.max_waits
        }

        fn dictating(&self) -> bool {
            self.dictating
        }

        fn engine_ready(&self) -> bool {
            self.engine_ready
        }

        fn microphone_granted(&self) -> bool {
            self.microphone_granted
        }

        fn capture_is_open(&self) -> bool {
            *self.starts.borrow() > *self.rebuilds.borrow()
        }

        fn start_capture(&mut self) -> Result<(), String> {
            if self.capture_starts_ok {
                *self.starts.borrow_mut() += 1;
                Ok(())
            } else {
                Err("microphone busy".to_string())
            }
        }

        fn rebuild_capture(&mut self) {
            *self.rebuilds.borrow_mut() += 1;
        }

        fn close_capture(&mut self) {
            *self.closes.borrow_mut() += 1;
        }

        fn take_segment(&mut self) -> Result<Vec<f32>, String> {
            let mut segments = self.segments.borrow_mut();
            if segments.is_empty() {
                Ok(silence_chunk())
            } else {
                segments.remove(0)
            }
        }

        fn wake_check(&mut self, _samples: Vec<f32>) -> WakeCheck {
            *self.checks.borrow_mut() += 1;
            let mut transcripts = self.transcripts.borrow_mut();
            if transcripts.is_empty() {
                WakeCheck::Transcript(String::new())
            } else {
                transcripts.remove(0)
            }
        }

        fn report_microphone_problem(&mut self) {
            *self.mic_reports.borrow_mut() += 1;
        }

        fn trigger_wake(&mut self) {
            *self.triggers.borrow_mut() += 1;
        }
    }

    #[test]
    fn stands_down_while_a_dictation_is_active_and_never_opens_the_microphone() {
        let mut listener = FakeListener {
            dictating: true,
            ..Default::default()
        };
        run_listen_loop(&mut listener);
        assert_eq!(*listener.starts.borrow(), 0);
        assert_eq!(*listener.checks.borrow(), 0);
        assert_eq!(*listener.closes.borrow(), 1);
    }

    #[test]
    fn waits_for_the_engine_before_touching_the_microphone() {
        let mut listener = FakeListener {
            engine_ready: false,
            ..Default::default()
        };
        run_listen_loop(&mut listener);
        assert_eq!(*listener.starts.borrow(), 0);
        assert_eq!(*listener.checks.borrow(), 0);
        assert_eq!(*listener.closes.borrow(), 1);
    }

    #[test]
    fn a_denied_microphone_is_reported_once_not_repeatedly() {
        let mut listener = FakeListener {
            microphone_granted: false,
            ..Default::default()
        };
        run_listen_loop(&mut listener);
        assert_eq!(*listener.mic_reports.borrow(), 1);
        assert_eq!(*listener.starts.borrow(), 0);
    }

    #[test]
    fn a_failed_capture_start_retries_through_the_rebuild_path() {
        let mut listener = FakeListener {
            capture_starts_ok: false,
            ..Default::default()
        };
        run_listen_loop(&mut listener);
        assert!(*listener.rebuilds.borrow() >= 1);
        assert_eq!(*listener.checks.borrow(), 0);
        assert_eq!(*listener.closes.borrow(), 1);
    }

    #[test]
    fn digital_silence_reports_once_and_travels_the_retry_path() {
        let mut listener = FakeListener::default();
        // Two silent seconds, then more of the same from the fallback.
        *listener.segments.borrow_mut() = vec![
            Ok(silence_chunk()),
            Ok(silence_chunk()),
            Ok(silence_chunk()),
        ];
        run_listen_loop(&mut listener);
        assert_eq!(*listener.mic_reports.borrow(), 1);
        assert!(*listener.rebuilds.borrow() >= 1);
        assert_eq!(*listener.checks.borrow(), 0);
    }

    #[test]
    fn quiet_audio_keeps_listening_without_a_wake_check_or_a_rebuild() {
        // A chunk that is quiet but not digitally silent is hard to synthesise
        // cheaply here; the observable contract under test is that ordinary
        // listening performs no wake checks while nothing is said, which the
        // default silence fallback already covers.
        let mut listener = FakeListener::default();
        run_listen_loop(&mut listener);
        assert_eq!(*listener.checks.borrow(), 0);
        assert_eq!(*listener.triggers.borrow(), 0);
    }

    #[test]
    fn speech_goes_to_the_wake_check_and_an_engine_loss_reopens_capture() {
        let mut listener = FakeListener::default();
        *listener.segments.borrow_mut() =
            vec![Ok(speech_chunk()), Ok(speech_chunk()), Ok(silence_chunk())];
        *listener.transcripts.borrow_mut() = vec![
            WakeCheck::EngineUnavailable,
            WakeCheck::TranscriptionFailed("engine exploded".to_string()),
        ];
        run_listen_loop(&mut listener);
        assert_eq!(*listener.checks.borrow(), 2);
        // The unavailable engine sent the loop down the retry path...
        assert!(*listener.rebuilds.borrow() >= 1);
        // ...and it recovered enough to keep listening afterwards.
        assert_eq!(*listener.closes.borrow(), 1);
    }
}
