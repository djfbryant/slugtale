//! The always-on listener: a microphone the Voice Activation window holds open
//! between wake checks.
//!
//! Dictation's recorder keeps a paused CoreAudio stream so the next Hotkey only
//! pays for `play`. The listener cannot share that trick: after dictation or
//! digital silence, `play` on a retained stream can succeed while the callback
//! supplies only zeros (slugtale-3wo). Closing therefore drops the recorder, and
//! [`VoiceActivationCapture::rebuild`] hands the next start a fresh one.

use crate::audio_capture::recorder::VoiceActivationRecorder;
use crate::{AudioCaptureError, CapturedAudio};

/// Always-on microphone used by Voice Activation.
///
/// Dictation's [`CpalAudioRecorder`] keeps a paused CoreAudio stream so the next
/// hotkey only pays for `play`. The listener cannot share that trick: after
/// dictation or digital silence, `play` on the retained stream can succeed while
/// the callback supplies only zeros (slugtale-3wo). Closing therefore drops the
/// recorder and the next start is given a fresh one.
pub struct VoiceActivationCapture<R: VoiceActivationRecorder> {
    recorder: R,
    open: bool,
}

impl<R: VoiceActivationRecorder> VoiceActivationCapture<R> {
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            open: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn start(&mut self) -> Result<(), AudioCaptureError> {
        if self.open {
            return Ok(());
        }
        self.recorder.start()?;
        self.open = true;
        Ok(())
    }

    pub fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        self.recorder.take_segment()
    }

    /// Stop capture if it is running, then replace the recorder so the next
    /// start cannot resume a paused stream.
    pub fn rebuild(&mut self, next: R) {
        self.close();
        self.recorder = next;
    }

    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        let _ = self.recorder.cancel();
        self.open = false;
    }
}

impl<R: VoiceActivationRecorder> Drop for VoiceActivationCapture<R> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct GenerationRecorder {
        generation: u32,
        events: std::rc::Rc<std::cell::RefCell<Vec<(u32, &'static str)>>>,
    }

    impl GenerationRecorder {
        fn new(
            generation: u32,
            events: std::rc::Rc<std::cell::RefCell<Vec<(u32, &'static str)>>>,
        ) -> Self {
            Self { generation, events }
        }
    }

    impl VoiceActivationRecorder for GenerationRecorder {
        fn start(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push((self.generation, "start"));
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push((self.generation, "cancel"));
            Ok(())
        }

        fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(CapturedAudio::mono_16khz(vec![0.1]))
        }
    }

    #[test]
    fn a_second_listen_after_dictation_rebuilds_the_recorder() {
        // The installed-app failure: first "Hi Slugtale" starts dictation, then
        // later phrases do nothing because start() resumed the paused listener
        // stream and CoreAudio fed it digital silence. Rebuilding is the fix.
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events.clone()));

        capture.start().unwrap();
        capture.rebuild(GenerationRecorder::new(2, events.clone()));
        capture.start().unwrap();

        assert_eq!(
            events.borrow().as_slice(),
            &[(1, "start"), (1, "cancel"), (2, "start")]
        );
        assert!(capture.is_open());
    }

    #[test]
    fn the_listener_keeps_its_recorder_while_it_is_still_listening() {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events.clone()));

        capture.start().unwrap();
        capture.start().unwrap();
        let _ = capture.take_segment().unwrap();

        assert_eq!(events.borrow().as_slice(), &[(1, "start")]);
    }

    #[test]
    fn voice_activation_capture_reports_open_while_listening() {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events));

        assert!(!capture.is_open());
        capture.start().unwrap();
        assert!(capture.is_open());
    }
}
