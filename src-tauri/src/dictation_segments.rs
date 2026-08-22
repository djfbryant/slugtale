//! Ordered Dictation Segment execution (ADR-0015).
//!
//! A Dictation Workflow completes one segment. This module owns the facts that
//! span segments: spoken order, first versus continuation text, cancellation,
//! rescue suppression, and the Counted Segment handoff. Keeping those facts
//! together makes the worker testable without Tauri or a microphone.

use crate::{
    count_words, CapturedAudio, CountedSegment, DictationSegmentOutcome, DictationSegmentPosition,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug)]
pub enum DictationSegmentJob {
    PauseFlush {
        dictation: u64,
    },
    Last {
        dictation: u64,
        audio: CapturedAudio,
    },
}

impl DictationSegmentJob {
    pub fn dictation(&self) -> u64 {
        match self {
            Self::PauseFlush { dictation } | Self::Last { dictation, .. } => *dictation,
        }
    }

    pub fn is_last(&self) -> bool {
        matches!(self, Self::Last { .. })
    }
}

/// Shared Dictation Segment state. The Tauri tier owns transport and audio;
/// this module decides which queued work is still valid.
#[derive(Default)]
pub struct DictationSegmentControl {
    dictation: AtomicU64,
    cancelled_through: AtomicU64,
    rescued: AtomicBool,
}

impl DictationSegmentControl {
    pub fn current(&self) -> u64 {
        self.dictation.load(Ordering::SeqCst)
    }

    pub fn begin(&self) -> u64 {
        self.rescued.store(false, Ordering::SeqCst);
        self.dictation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn abandon(&self) {
        self.cancelled_through
            .store(self.current(), Ordering::SeqCst);
    }

    pub fn is_cancelled(&self, dictation: u64) -> bool {
        dictation <= self.cancelled_through.load(Ordering::SeqCst)
    }

    pub fn is_recording(&self, dictation: u64) -> bool {
        self.current() == dictation && !self.is_cancelled(dictation)
    }

    pub fn suspend_pause_flushes(&self) {
        self.rescued.store(true, Ordering::SeqCst);
    }

    pub fn pause_flushes_suspended(&self) -> bool {
        self.rescued.load(Ordering::SeqCst)
    }
}

/// The app-specific operations that ordered Dictation Segment execution needs.
/// This is an internal seam for the module's tests and for the Tauri worker.
pub trait DictationSegmentExecution {
    type Error;

    fn take_pause_segment(&mut self) -> Option<CapturedAudio>;
    fn complete(
        &mut self,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, Self::Error>;
    fn record(&mut self, segment: CountedSegment);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationSegmentJobResult {
    Skipped {
        last: bool,
    },
    Completed {
        last: bool,
        inserted: bool,
        rescued: bool,
        text_chars: usize,
    },
}

/// A single-worker state machine. One worker holds this for the app lifetime,
/// which is what prevents a later Dictation Segment from overtaking an earlier
/// one when transcription takes longer.
#[derive(Default)]
pub struct DictationSegmentWorker {
    dictation: u64,
    inserted_any: bool,
}

impl DictationSegmentWorker {
    pub fn process<E: DictationSegmentExecution>(
        &mut self,
        job: DictationSegmentJob,
        control: &DictationSegmentControl,
        execution: &mut E,
    ) -> Result<DictationSegmentJobResult, E::Error> {
        let number = job.dictation();
        let last = job.is_last();
        if number != self.dictation {
            self.dictation = number;
            self.inserted_any = false;
        }

        let audio = match job {
            DictationSegmentJob::PauseFlush { .. } => {
                if control.pause_flushes_suspended() || !control.is_recording(number) {
                    None
                } else {
                    execution.take_pause_segment()
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
        let outcome = execution.complete(audio, position)?;
        if outcome.inserted {
            execution.record(CountedSegment {
                words: count_words(&outcome.transcription.text),
                speaking_seconds,
                starts_dictation,
            });
        }
        self.inserted_any |= outcome.inserted;
        if outcome.rescued {
            control.suspend_pause_flushes();
        }

        Ok(DictationSegmentJobResult::Completed {
            last,
            inserted: outcome.inserted,
            rescued: outcome.rescued,
            text_chars: outcome.transcription.text.chars().count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinalTranscription, TranscriptSegment};

    #[derive(Default)]
    struct FakeExecution {
        audio: Vec<CapturedAudio>,
        outcomes: Vec<DictationSegmentOutcome>,
        positions: Vec<DictationSegmentPosition>,
        recorded: Vec<CountedSegment>,
    }

    impl DictationSegmentExecution for FakeExecution {
        type Error = ();

        fn take_pause_segment(&mut self) -> Option<CapturedAudio> {
            (!self.audio.is_empty()).then(|| self.audio.remove(0))
        }

        fn complete(
            &mut self,
            _audio: CapturedAudio,
            position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, Self::Error> {
            self.positions.push(position);
            Ok(self.outcomes.remove(0))
        }

        fn record(&mut self, segment: CountedSegment) {
            self.recorded.push(segment);
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

    #[test]
    fn keeps_segments_ordered_and_makes_later_text_a_continuation() {
        let control = DictationSegmentControl::default();
        let dictation = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut execution = FakeExecution {
            audio: vec![audio(16_000, 16_000)],
            outcomes: vec![
                outcome("first words", true, false),
                outcome("next words", true, false),
            ],
            ..Default::default()
        };

        worker
            .process(
                DictationSegmentJob::PauseFlush { dictation },
                &control,
                &mut execution,
            )
            .unwrap();
        worker
            .process(
                DictationSegmentJob::Last {
                    dictation,
                    audio: audio(8_000, 16_000),
                },
                &control,
                &mut execution,
            )
            .unwrap();

        assert_eq!(
            execution.positions,
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::Continuation
            ]
        );
        assert_eq!(execution.recorded.len(), 2);
        assert!(execution.recorded[0].starts_dictation);
        assert!(!execution.recorded[1].starts_dictation);
        assert_eq!(execution.recorded[0].speaking_seconds, 1.0);
        assert_eq!(execution.recorded[1].speaking_seconds, 0.5);
    }

    #[test]
    fn a_silent_segment_does_not_consume_the_first_position() {
        let control = DictationSegmentControl::default();
        let dictation = control.begin();
        let mut worker = DictationSegmentWorker::default();
        let mut execution = FakeExecution {
            audio: vec![audio(1, 1)],
            outcomes: vec![outcome("", false, false), outcome("words", true, false)],
            ..Default::default()
        };

        worker
            .process(
                DictationSegmentJob::PauseFlush { dictation },
                &control,
                &mut execution,
            )
            .unwrap();
        worker
            .process(
                DictationSegmentJob::Last {
                    dictation,
                    audio: audio(1, 1),
                },
                &control,
                &mut execution,
            )
            .unwrap();

        assert_eq!(
            execution.positions,
            [
                DictationSegmentPosition::First,
                DictationSegmentPosition::First
            ]
        );
        assert_eq!(execution.recorded.len(), 1);
        assert!(execution.recorded[0].starts_dictation);
    }

    #[test]
    fn cancellation_and_rescue_suppress_pause_flushes_but_not_the_last_segment() {
        let control = DictationSegmentControl::default();
        let dictation = control.begin();
        control.suspend_pause_flushes();
        let mut worker = DictationSegmentWorker::default();
        let mut execution = FakeExecution {
            audio: vec![audio(1, 1)],
            outcomes: vec![outcome("rescued", true, true)],
            ..Default::default()
        };

        assert_eq!(
            worker
                .process(
                    DictationSegmentJob::PauseFlush { dictation },
                    &control,
                    &mut execution
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
                &mut execution,
            )
            .unwrap();
        assert_eq!(execution.positions, [DictationSegmentPosition::First]);

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
                    &mut execution
                )
                .unwrap(),
            DictationSegmentJobResult::Skipped { last: true }
        );
    }
}
