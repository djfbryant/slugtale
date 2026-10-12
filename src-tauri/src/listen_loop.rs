
use crate::{wake_phrase_score, SpeechWindowBuffer, WakeWordConfig, WakeWordDetector};
use std::time::Duration;

pub const LISTEN_POLL: Duration = Duration::from_millis(250);
pub const LISTEN_CAPTURE_RETRY: Duration = Duration::from_secs(2);

const MIN_NEW_SAMPLES: usize = 32_000;
const OVERLAP_SAMPLES: usize = 16_000;
const SPEECH_FRAME_SAMPLES: usize = 320;
const MINIMUM_SPEECH_RMS: f32 = 0.006;
const SPEECH_CONTRAST_RATIO: f32 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerCommand {
    Listen,
    Stop,
}

#[derive(Debug, PartialEq)]
pub enum WakeCheck {
    EngineUnavailable,
    TranscriptionFailed(String),
    Transcript(String),
}

pub trait ListenTransport {
    fn next_command(&mut self) -> Option<ListenerCommand>;

    fn stop_requested(&self) -> bool;

    fn wait(&mut self, timeout: Duration) -> bool;

    fn dictating(&self) -> bool;
}

pub trait WakeCapture {
    fn engine_ready(&self) -> bool;

    fn microphone_granted(&self) -> bool;

    fn capture_is_open(&self) -> bool;
    fn start_capture(&mut self) -> Result<(), String>;
    fn rebuild_capture(&mut self);
    fn close_capture(&mut self);
    fn take_segment(&mut self) -> Result<Vec<f32>, String>;
}

pub trait WakeTrigger {
    /// Run one wake check over a window of speech. Always greedy decoding:
    /// the user's wider beam is useful for dictation text, but wasteful for a
    /// two-word phrase.
    fn wake_check(&mut self, samples: Vec<f32>) -> WakeCheck;

    fn report_microphone_problem(&mut self);

    fn trigger_wake(&mut self);
}

pub trait WakeListener: ListenTransport + WakeCapture + WakeTrigger {}

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

    fn speech_chunk() -> Vec<f32> {
        let frame = 320;
        let mut samples = vec![0.0; 32_000];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = if index / frame >= 75 { 0.4 } else { 0.000_1 };
        }
        samples
    }

    fn silence_chunk() -> Vec<f32> {
        vec![0.0; 32_000]
    }

    impl ListenTransport for FakeListener {
        fn next_command(&mut self) -> Option<ListenerCommand> {
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
    }

    impl WakeCapture for FakeListener {
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
    }

    impl WakeTrigger for FakeListener {
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

    impl WakeListener for FakeListener {}

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
        assert!(*listener.rebuilds.borrow() >= 1);
        assert_eq!(*listener.closes.borrow(), 1);
    }
}
