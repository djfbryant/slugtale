//! Dictation Runtime: a recording that came back as digital silence is a denied
//! microphone, not a quiet room, and it must be refused before a transcription
//! engine is asked to hear "You" out of a buffer of zeros (slugtale-d3k).

use crate::audio_capture::recorder::{AudioLevelCallback, DictationRecorder};
use crate::audio_capture::signal::require_captured_microphone_signal;
use crate::{AudioCaptureError, CapturedAudio, DictationEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum AudioCaptureOutcome {
    Completed(CapturedAudio),
    Discarded,
}

pub struct AudioCaptureSession<R> {
    recorder: R,
    active: bool,
    flushed_a_segment: bool,
}

impl<R> AudioCaptureSession<R>
where
    R: DictationRecorder,
{
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            active: false,
            flushed_a_segment: false,
        }
    }

    pub fn prepare(&mut self) -> Result<(), AudioCaptureError> {
        self.recorder.prepare()
    }

    pub fn set_level_callback(&mut self, callback: Option<AudioLevelCallback>) {
        self.recorder.set_level_callback(callback);
    }

    pub fn set_prefer_built_in_microphone(&mut self, prefer: bool) {
        self.recorder.set_prefer_built_in_microphone(prefer);
    }

    /// Take the speech captured so far as a Dictation Segment, leaving the
    /// recording running. Only audio through `cut` — the watermark this Pause
    /// Flush was queued with — joins the segment, so a slow worker cannot append
    /// later speech to it (slugtale-g1o.4).
    ///
    /// Returns `None` when there is nothing to take: either no dictation is
    /// active, or the ring has been drained since the last Segment Pause.
    pub fn cut_segment(&mut self, cut: u64) -> Result<Option<CapturedAudio>, AudioCaptureError> {
        if !self.active {
            return Ok(None);
        }

        let audio = self.recorder.cut_segment(cut)?;
        if audio.samples.is_empty() {
            return Ok(None);
        }

        self.flushed_a_segment = true;
        Ok(Some(audio))
    }

    pub fn on_event(
        &mut self,
        event: DictationEvent,
    ) -> Result<Option<AudioCaptureOutcome>, AudioCaptureError> {
        match event {
            DictationEvent::Start => {
                self.recorder.start()?;
                self.active = true;
                self.flushed_a_segment = false;
                Ok(None)
            }
            DictationEvent::Stop if self.active => {
                self.active = false;
                let audio = self.recorder.stop()?;
                if !self.flushed_a_segment {
                    require_captured_microphone_signal(&audio)?;
                }
                Ok(Some(AudioCaptureOutcome::Completed(audio)))
            }
            DictationEvent::Cancel if self.active => {
                self.active = false;
                self.recorder.cancel()?;
                Ok(Some(AudioCaptureOutcome::Discarded))
            }
            DictationEvent::Stop | DictationEvent::Cancel => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Arc;

    #[test]
    fn audio_capture_session_stops_with_captured_samples_for_transcription() {
        let log = Rc::new(RecorderLog::default());
        let recorder = FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.0, 0.2, -0.2]),
            log.clone(),
        );
        let mut session = AudioCaptureSession::new(recorder);

        assert_eq!(session.on_event(DictationEvent::Start).unwrap(), None);
        let completed = session.on_event(DictationEvent::Stop).unwrap();

        assert_eq!(
            completed,
            Some(AudioCaptureOutcome::Completed(CapturedAudio::mono_16khz(
                vec![0.0, 0.2, -0.2]
            )))
        );
        assert_eq!(log.events.borrow().as_slice(), ["start", "stop"]);
    }

    #[test]
    fn audio_capture_session_rejects_digital_silence_before_transcription() {
        let log = Rc::new(RecorderLog::default());
        let recorder =
            FakeDictationRecorder::new(CapturedAudio::mono_16khz(vec![0.0; 80_000]), log.clone());
        let mut session = AudioCaptureSession::new(recorder);

        session.on_event(DictationEvent::Start).unwrap();
        let error = session.on_event(DictationEvent::Stop).unwrap_err();

        assert_eq!(
            error,
            AudioCaptureError::new(
                "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone"
            )
        );
        assert_eq!(log.events.borrow().as_slice(), ["start", "stop"]);
    }

    #[test]
    fn cutting_a_segment_keeps_the_recording_running_for_the_next_one() {
        let log = Rc::new(RecorderLog::default());
        let recorder = FakeDictationRecorder::with_pending_segments(
            CapturedAudio::mono_16khz(vec![0.3, 0.3]),
            vec![
                CapturedAudio::mono_16khz(vec![0.1, 0.1]),
                CapturedAudio::mono_16khz(vec![0.2, 0.2]),
            ],
            log.clone(),
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        let first = session.cut_segment(4_000).unwrap();
        let second = session.cut_segment(8_000).unwrap();
        let remainder = session.on_event(DictationEvent::Stop).unwrap();

        assert_eq!(first, Some(CapturedAudio::mono_16khz(vec![0.1, 0.1])));
        assert_eq!(second, Some(CapturedAudio::mono_16khz(vec![0.2, 0.2])));
        assert_eq!(
            remainder,
            Some(AudioCaptureOutcome::Completed(CapturedAudio::mono_16khz(
                vec![0.3, 0.3]
            )))
        );
        assert_eq!(
            log.events.borrow().as_slice(),
            ["start", "cut_segment", "cut_segment", "stop"]
        );
    }

    #[test]
    fn cutting_a_drained_ring_yields_no_segment() {
        let recorder = FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.2]),
            Rc::new(RecorderLog::default()),
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        assert_eq!(session.cut_segment(0).unwrap(), None);
    }

    #[test]
    fn cutting_outside_a_dictation_yields_no_segment() {
        let log = Rc::new(RecorderLog::default());
        let recorder = FakeDictationRecorder::with_pending_segments(
            CapturedAudio::mono_16khz(vec![0.2]),
            vec![CapturedAudio::mono_16khz(vec![0.1])],
            log.clone(),
        );
        let mut session = AudioCaptureSession::new(recorder);

        assert_eq!(session.cut_segment(0).unwrap(), None);
        assert!(log.events.borrow().is_empty());
    }

    #[test]
    fn a_pause_flush_hands_its_own_cut_to_the_recorder() {
        let log = Rc::new(RecorderLog::default());
        let mut session = AudioCaptureSession::new(FakeDictationRecorder::with_pending_segments(
            CapturedAudio::mono_16khz(vec![0.1]),
            vec![CapturedAudio::mono_16khz(vec![0.2, 0.2])],
            log.clone(),
        ));
        session.on_event(DictationEvent::Start).unwrap();

        assert!(session.cut_segment(48_000).unwrap().is_some());

        assert_eq!(log.cuts.borrow().as_slice(), [48_000]);
    }

    #[test]
    fn a_recorder_publishes_one_watermark_cell_for_its_whole_life() {
        let recorder = FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.1]),
            Rc::new(RecorderLog::default()),
        );

        let first = recorder.voice_watermark_cell();
        let second = recorder.voice_watermark_cell();

        assert!(
            Arc::ptr_eq(&first, &second),
            "the cell must survive for the recorder's whole life, or the host would read a dead one"
        );
        assert_eq!(first.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn the_level_callback_reaches_the_recorder_and_is_cleared_on_the_session() {
        let log = Rc::new(RecorderLog::default());
        let mut session = AudioCaptureSession::new(FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.1]),
            log.clone(),
        ));

        session.set_level_callback(Some(std::sync::Arc::new(|_| {})));
        session.set_level_callback(None);

        assert_eq!(
            log.events.borrow().as_slice(),
            ["level_callback", "level_callback_off"]
        );
    }

    #[test]
    fn preparing_twice_prepares_once_and_start_does_not_reprepare() {
        let log = Rc::new(RecorderLog::default());
        let mut session = AudioCaptureSession::new(FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.2]),
            log.clone(),
        ));
        session.prepare().unwrap();
        session.prepare().unwrap();
        session.on_event(DictationEvent::Start).unwrap();

        assert_eq!(log.events.borrow().as_slice(), ["prepare", "start"]);
    }

    #[test]
    fn a_failed_prepare_does_not_block_the_next_dictation_start() {
        let recorder = FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.2]),
            Rc::new(RecorderLog::default()),
        )
        .failing_prepare();
        let mut session = AudioCaptureSession::new(recorder);

        assert!(session.prepare().is_err());

        assert_eq!(session.on_event(DictationEvent::Start).unwrap(), None);
        let completed = session.on_event(DictationEvent::Stop).unwrap();
        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
    }

    #[test]
    fn preparing_a_recording_recorder_changes_nothing() {
        let log = Rc::new(RecorderLog::default());
        let mut session = AudioCaptureSession::new(FakeDictationRecorder::new(
            CapturedAudio::mono_16khz(vec![0.2]),
            log.clone(),
        ));
        session.on_event(DictationEvent::Start).unwrap();

        session.prepare().unwrap();

        let completed = session.on_event(DictationEvent::Stop).unwrap();
        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
        assert_eq!(log.events.borrow().as_slice(), ["start", "stop"]);
    }

    #[test]
    fn a_dictation_that_already_flushed_speech_may_end_on_silence() {
        let recorder = FakeDictationRecorder::with_pending_segments(
            CapturedAudio::mono_16khz(vec![0.0; 16_000]),
            vec![CapturedAudio::mono_16khz(vec![0.4, -0.4])],
            Rc::new(RecorderLog::default()),
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();
        session.cut_segment(0).unwrap();

        let completed = session.on_event(DictationEvent::Stop).unwrap();

        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
    }

    #[test]
    fn a_new_dictation_restores_the_digital_silence_guard() {
        let recorder = FakeDictationRecorder::with_pending_segments(
            CapturedAudio::mono_16khz(vec![0.0; 16_000]),
            vec![CapturedAudio::mono_16khz(vec![0.4, -0.4])],
            Rc::new(RecorderLog::default()),
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();
        session.cut_segment(0).unwrap();
        session.on_event(DictationEvent::Stop).unwrap();

        session.on_event(DictationEvent::Start).unwrap();
        let error = session.on_event(DictationEvent::Stop).unwrap_err();

        assert_eq!(
            error,
            AudioCaptureError::new(
                "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone"
            )
        );
    }

    #[test]
    fn audio_capture_session_cancel_discards_without_returning_audio() {
        let log = Rc::new(RecorderLog::default());
        let recorder =
            FakeDictationRecorder::new(CapturedAudio::mono_16khz(vec![0.4, 0.5]), log.clone());
        let mut session = AudioCaptureSession::new(recorder);

        session.on_event(DictationEvent::Start).unwrap();
        let discarded = session.on_event(DictationEvent::Cancel).unwrap();

        assert_eq!(discarded, Some(AudioCaptureOutcome::Discarded));
        assert_eq!(log.events.borrow().as_slice(), ["start", "cancel"]);
    }

    #[derive(Default)]
    struct RecorderLog {
        events: RefCell<Vec<&'static str>>,
        cuts: RefCell<Vec<u64>>,
        watermark: std::sync::Arc<std::sync::atomic::AtomicU64>,
    }

    struct FakeDictationRecorder {
        audio: CapturedAudio,
        segments: RefCell<VecDeque<CapturedAudio>>,
        log: Rc<RecorderLog>,
        prepared_or_recording: Cell<bool>,
        fail_prepare: bool,
    }

    impl FakeDictationRecorder {
        fn new(audio: CapturedAudio, log: Rc<RecorderLog>) -> Self {
            Self {
                audio,
                segments: RefCell::new(VecDeque::new()),
                log,
                prepared_or_recording: Cell::new(false),
                fail_prepare: false,
            }
        }

        fn with_pending_segments(
            audio: CapturedAudio,
            segments: Vec<CapturedAudio>,
            log: Rc<RecorderLog>,
        ) -> Self {
            let recorder = Self::new(audio, log);
            *recorder.segments.borrow_mut() = segments.into();
            recorder
        }

        fn failing_prepare(mut self) -> Self {
            self.fail_prepare = true;
            self
        }
    }

    impl DictationRecorder for FakeDictationRecorder {
        fn prepare(&mut self) -> Result<(), AudioCaptureError> {
            if self.fail_prepare {
                return Err(AudioCaptureError::new("fake prepare failure"));
            }
            if !self.prepared_or_recording.replace(true) {
                self.log.events.borrow_mut().push("prepare");
            }
            Ok(())
        }

        fn start(&mut self) -> Result<(), AudioCaptureError> {
            self.log.events.borrow_mut().push("start");
            self.prepared_or_recording.set(true);
            Ok(())
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            self.log.events.borrow_mut().push("stop");
            self.prepared_or_recording.set(false);
            Ok(self.audio.clone())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.log.events.borrow_mut().push("cancel");
            self.prepared_or_recording.set(false);
            Ok(())
        }

        fn cut_segment(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            self.log.events.borrow_mut().push("cut_segment");
            self.log.cuts.borrow_mut().push(cut);
            Ok(self
                .segments
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| CapturedAudio::mono_16khz(Vec::new())))
        }

        fn voice_watermark_cell(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
            std::sync::Arc::clone(&self.log.watermark)
        }

        fn set_level_callback(&mut self, callback: Option<AudioLevelCallback>) {
            let event = match callback {
                Some(_) => "level_callback",
                None => "level_callback_off",
            };
            self.log.events.borrow_mut().push(event);
        }
    }
}
