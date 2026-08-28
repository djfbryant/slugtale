use crate::{
    transcribe_captured_audio, AsrError, AsrRuntime, CapturedAudio, FinalTranscription,
    InsertionRescue, InsertionRescueError, TextInsertion, TranscriptCleanupMode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationWorkflowError {
    Transcription(AsrError),
    InsertionRescue(InsertionRescueError),
}

impl std::fmt::Display for DictationWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transcription(error) => write!(f, "{error}"),
            Self::InsertionRescue(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DictationWorkflowError {}

/// Where a Dictation Segment sits in its dictation, which is the whole of what
/// Transcript Cleanup needs to know to join it onto what came before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationSegmentPosition {
    /// The first text this dictation will put into the text target.
    First,
    /// Text appended after something this dictation has already inserted.
    Continuation,
}

/// What one run of the Dictation Workflow did. A dictation now produces one of
/// these per Dictation Segment rather than exactly one per dictation, so the
/// caller needs more than the text back: it has to know whether anything was
/// actually inserted (to place the next segment) and whether the Insertion
/// Rescue was involved (because a rescue once per Segment Pause would clobber
/// the clipboard and notify the user every few seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationSegmentOutcome {
    pub transcription: FinalTranscription,
    /// Whether this segment put text into the text target, directly or through
    /// the Insertion Rescue. A segment that transcribed to nothing inserts
    /// nothing, which leaves the next segment still the dictation's first.
    pub inserted: bool,
    /// Whether Text Insertion failed and the Insertion Rescue took over.
    pub rescued: bool,
}

pub struct DictationWorkflow<'a> {
    runtime: &'a dyn AsrRuntime,
    text_insertion: &'a dyn TextInsertion,
    insertion_rescue: &'a dyn InsertionRescue,
    transcript_cleanup: TranscriptCleanupMode,
}

impl<'a> DictationWorkflow<'a> {
    pub fn new(
        runtime: &'a dyn AsrRuntime,
        text_insertion: &'a dyn TextInsertion,
        insertion_rescue: &'a dyn InsertionRescue,
        transcript_cleanup: TranscriptCleanupMode,
    ) -> Self {
        Self {
            runtime,
            text_insertion,
            insertion_rescue,
            transcript_cleanup,
        }
    }

    /// Transcribe one Dictation Segment, clean it, and insert it at the caret.
    ///
    /// `position` says whether this is the dictation's opening text or an
    /// append after an earlier Segment Pause already inserted some.
    pub fn complete(
        &self,
        audio: CapturedAudio,
        position: DictationSegmentPosition,
    ) -> Result<DictationSegmentOutcome, DictationWorkflowError> {
        let transcription = transcribe_captured_audio(self.runtime, audio)
            .map_err(DictationWorkflowError::Transcription)?;
        let transcription =
            clean_dictation_segment(transcription, position, self.transcript_cleanup);

        // A segment that heard nothing must not be inserted. Before Segment
        // Pauses this only mattered for a whole silent dictation; now a user who
        // pauses, coughs, and pauses again would otherwise get a stray space
        // typed into their document.
        if transcription.text.trim().is_empty() {
            return Ok(DictationSegmentOutcome {
                transcription,
                inserted: false,
                rescued: false,
            });
        }

        let mut rescued = false;
        if self.text_insertion.insert(&transcription).is_err() {
            rescued = true;
            self.insertion_rescue
                .rescue(&transcription)
                .map_err(DictationWorkflowError::InsertionRescue)?;
        }

        Ok(DictationSegmentOutcome {
            transcription,
            inserted: true,
            rescued,
        })
    }
}

/// Apply deterministic Transcript Cleanup to a Dictation Segment before
/// insertion, without rewriting meaning or adding generated text.
///
/// The two positions differ in exactly two ways, and both exist because the
/// transcription engine treats every segment as a fresh utterance:
///
/// - A continuation is prefixed with one space, because the engine's text starts
///   flush against whatever is already in the document.
/// - A continuation keeps whatever casing the engine produced. The first segment
///   still gets its first letter capitalised, but doing that mid-dictation would
///   force a capital onto speech that carried on from the previous sentence.
///
/// The cleanup mode selects how much runs (slugtale-kyc): Basic is whitespace
/// normalization only, while Clean Dictation additionally removes safe filler
/// words such as "um" before the position rules apply. Clean Dictation with
/// Pause Breaks also uses the preserved segment timings to add conservative
/// line breaks.
pub fn clean_dictation_segment(
    transcription: FinalTranscription,
    position: DictationSegmentPosition,
    cleanup: TranscriptCleanupMode,
) -> FinalTranscription {
    let normalized = match cleanup {
        TranscriptCleanupMode::Basic => crate::normalize_transcript_whitespace(&transcription.text),
        TranscriptCleanupMode::CleanDictation => crate::remove_filler_words(&transcription.text),
        TranscriptCleanupMode::CleanDictationWithPauseBreaks => {
            if transcription.segments.is_empty() {
                crate::remove_filler_words(&transcription.text)
            } else {
                crate::clean_with_pause_line_breaks(&transcription.segments)
            }
        }
    };
    if normalized.is_empty() {
        return FinalTranscription {
            text: normalized,
            segments: transcription.segments,
        };
    }

    let text = match position {
        DictationSegmentPosition::Continuation => format!(" {normalized}"),
        DictationSegmentPosition::First => {
            let mut chars = normalized.chars();
            let first = chars.next().expect("normalized text is not empty");
            if first.is_lowercase() {
                format!("{}{}", first.to_uppercase(), chars.as_str())
            } else {
                normalized
            }
        }
    };

    FinalTranscription {
        text,
        segments: transcription.segments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InsertionRescueOutcome, TextInsertionError, TextInsertionOutcome};

    #[test]
    fn dictation_workflow_cleans_final_transcription_before_immediate_insertion() {
        let runtime = FakeAsrRuntime::new("  hello   from slugtale  ");
        let insertion = FakeTextInsertion::default();
        let rescue = FakeInsertionRescue::default();
        let workflow =
            DictationWorkflow::new(&runtime, &insertion, &rescue, TranscriptCleanupMode::Basic);

        let outcome = workflow
            .complete(
                CapturedAudio::mono_16khz(vec![0.0, 0.25]),
                DictationSegmentPosition::First,
            )
            .unwrap();

        assert_eq!(outcome.transcription.text, "Hello from slugtale");
        assert!(outcome.inserted);
        assert!(!outcome.rescued);
        assert_eq!(
            insertion.inserted.borrow().as_slice(),
            &["Hello from slugtale"]
        );
    }

    #[test]
    fn a_continuation_segment_appends_after_the_text_already_inserted() {
        // Whisper punctuates and capitalises each segment as its own utterance,
        // so the only thing missing when appending is the separating space.
        let runtime = FakeAsrRuntime::new("This is the second paragraph.");
        let insertion = FakeTextInsertion::default();
        let rescue = FakeInsertionRescue::default();
        let workflow =
            DictationWorkflow::new(&runtime, &insertion, &rescue, TranscriptCleanupMode::Basic);

        let outcome = workflow
            .complete(
                CapturedAudio::mono_16khz(vec![0.0, 0.25]),
                DictationSegmentPosition::Continuation,
            )
            .unwrap();

        assert_eq!(outcome.transcription.text, " This is the second paragraph.");
        assert!(outcome.inserted);
        assert_eq!(
            insertion.inserted.borrow().as_slice(),
            &[" This is the second paragraph."]
        );
    }

    #[test]
    fn a_segment_that_heard_nothing_inserts_nothing() {
        // Otherwise a Segment Pause over a cough would type a bare space into
        // the user's document, and would count as inserted text for the next
        // segment's spacing.
        let runtime = FakeAsrRuntime::new("   ");
        let insertion = FakeTextInsertion::default();
        let rescue = FakeInsertionRescue::default();
        let workflow =
            DictationWorkflow::new(&runtime, &insertion, &rescue, TranscriptCleanupMode::Basic);

        for position in [
            DictationSegmentPosition::First,
            DictationSegmentPosition::Continuation,
        ] {
            let outcome = workflow
                .complete(CapturedAudio::mono_16khz(vec![0.0]), position)
                .unwrap();

            assert!(!outcome.inserted, "{position:?} should not insert");
            assert!(!outcome.rescued);
        }

        assert!(insertion.inserted.borrow().is_empty());
        assert!(rescue.rescued.borrow().is_empty());
    }
    #[test]
    fn dictation_workflow_rescues_cleaned_transcription_when_insertion_fails() {
        let runtime = FakeAsrRuntime::new("  rescue   this transcription ");
        let insertion = FakeTextInsertion::fails();
        let rescue = FakeInsertionRescue::default();
        let workflow =
            DictationWorkflow::new(&runtime, &insertion, &rescue, TranscriptCleanupMode::Basic);

        let outcome = workflow
            .complete(
                CapturedAudio::mono_16khz(vec![0.0]),
                DictationSegmentPosition::First,
            )
            .unwrap();

        assert_eq!(outcome.transcription.text, "Rescue this transcription");
        assert!(outcome.inserted);
        // The caller suspends further Segment Pauses on this, so a dictation
        // without Accessibility trust does not clobber the clipboard and notify
        // once every five seconds.
        assert!(outcome.rescued);
        assert_eq!(
            rescue.rescued.borrow().as_slice(),
            &["Rescue this transcription"]
        );
    }

    #[test]
    fn continuation_cleanup_keeps_the_engine_casing_and_adds_one_space() {
        // A continuation carries on from speech that may not have ended a
        // sentence, so forcing a capital here would corrupt the reading.
        let cleaned = clean_dictation_segment(
            FinalTranscription::plain("  and then   we left  ".to_string()),
            DictationSegmentPosition::Continuation,
            TranscriptCleanupMode::Basic,
        );

        assert_eq!(cleaned.text, " and then we left");
    }

    #[test]
    fn clean_dictation_removes_fillers_before_the_position_rules_apply() {
        // The dropped opening filler leaves lowercase text, which the
        // first-segment capitalization rule then lifts as usual.
        let cleaned = clean_dictation_segment(
            FinalTranscription::plain("Um,   hello  from slugtale".to_string()),
            DictationSegmentPosition::First,
            TranscriptCleanupMode::CleanDictation,
        );

        assert_eq!(cleaned.text, "Hello from slugtale");
    }

    #[test]
    fn clean_dictation_never_damages_meaningful_words() {
        // "like" carries meaning; filler cleanup must prefer leaving it alone
        // over ever guessing wrong about it.
        let cleaned = clean_dictation_segment(
            FinalTranscription::plain("I like coffee".to_string()),
            DictationSegmentPosition::First,
            TranscriptCleanupMode::CleanDictation,
        );

        assert_eq!(cleaned.text, "I like coffee");
    }

    #[test]
    fn pause_break_mode_inserts_plain_text_line_breaks_from_asr_timing() {
        let cleaned = clean_dictation_segment(
            FinalTranscription::from_segments(vec![
                crate::TranscriptSegment {
                    text: "shopping list".to_string(),
                    start_ms: 0,
                    end_ms: 600,
                },
                crate::TranscriptSegment {
                    text: "milk and bread".to_string(),
                    start_ms: 2_400,
                    end_ms: 3_000,
                },
            ]),
            DictationSegmentPosition::First,
            TranscriptCleanupMode::CleanDictationWithPauseBreaks,
        );

        assert_eq!(cleaned.text, "Shopping list\nmilk and bread");
    }

    #[test]
    fn a_segment_that_is_all_filler_inserts_nothing() {
        let runtime = FakeAsrRuntime::new("um... uh");
        let insertion = FakeTextInsertion::default();
        let rescue = FakeInsertionRescue::default();
        let workflow = DictationWorkflow::new(
            &runtime,
            &insertion,
            &rescue,
            TranscriptCleanupMode::CleanDictation,
        );

        let outcome = workflow
            .complete(
                CapturedAudio::mono_16khz(vec![0.0]),
                DictationSegmentPosition::First,
            )
            .unwrap();

        assert!(!outcome.inserted);
        assert!(insertion.inserted.borrow().is_empty());
    }

    #[test]
    fn continuation_cleanup_of_silence_stays_empty_rather_than_a_bare_space() {
        let cleaned = clean_dictation_segment(
            FinalTranscription::plain("   ".to_string()),
            DictationSegmentPosition::Continuation,
            TranscriptCleanupMode::Basic,
        );

        assert_eq!(cleaned.text, "");
    }

    #[test]
    fn cleanup_preserves_asr_segments_unchanged_alongside_the_flattened_text() {
        // The flattened text is normalized for insertion while the raw segment
        // text and timing ride along untouched, so the later pause-aware
        // cleanup (slugtale-gnx) still sees the ASR boundaries.
        let transcription = FinalTranscription::from_segments(vec![
            crate::TranscriptSegment {
                text: " Hello ".to_string(),
                start_ms: 0,
                end_ms: 1_500,
            },
            crate::TranscriptSegment {
                text: " from slugtale.".to_string(),
                start_ms: 1_600,
                end_ms: 3_200,
            },
        ]);

        let cleaned = clean_dictation_segment(
            transcription,
            DictationSegmentPosition::First,
            TranscriptCleanupMode::Basic,
        );

        assert_eq!(cleaned.text, "Hello from slugtale.");
        assert_eq!(cleaned.segments.len(), 2);
        assert_eq!(cleaned.segments[0].text, " Hello ");
        assert_eq!(cleaned.segments[0].start_ms, 0);
        assert_eq!(cleaned.segments[1].end_ms, 3_200);
    }
    #[test]
    fn clean_final_transcription_trims_and_normalizes_repeated_spaces() {
        let transcription = clean_dictation_segment(
            FinalTranscription::plain("  hello   from    slugtale  ".to_string()),
            DictationSegmentPosition::First,
            TranscriptCleanupMode::Basic,
        );

        assert_eq!(transcription.text, "Hello from slugtale");
    }

    #[test]
    fn basic_cleanup_preserves_existing_line_breaks() {
        let transcription = clean_dictation_segment(
            FinalTranscription::plain("  shopping  list \n milk and bread ".to_string()),
            DictationSegmentPosition::First,
            TranscriptCleanupMode::Basic,
        );

        assert_eq!(transcription.text, "Shopping list\nmilk and bread");
    }
    #[test]
    fn clean_final_transcription_handles_empty_and_non_lowercase_starts() {
        let cases = [
            ("   ", ""),
            ("Already clean", "Already clean"),
            ("123 start recording", "123 start recording"),
            ("? question", "? question"),
        ];

        for (input, expected) in cases {
            let transcription = clean_dictation_segment(
                FinalTranscription::plain(input),
                DictationSegmentPosition::First,
                TranscriptCleanupMode::Basic,
            );

            assert_eq!(transcription.text, expected);
        }
    }

    struct FakeAsrRuntime {
        text: &'static str,
        sample_counts: std::cell::RefCell<Vec<usize>>,
    }

    impl FakeAsrRuntime {
        fn new(text: &'static str) -> Self {
            Self {
                text,
                sample_counts: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl AsrRuntime for FakeAsrRuntime {
        fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
            self.sample_counts.borrow_mut().push(audio.samples.len());
            Ok(FinalTranscription::plain(self.text))
        }
    }

    #[derive(Default)]
    struct FakeTextInsertion {
        inserted: std::cell::RefCell<Vec<String>>,
        fails: bool,
    }

    impl FakeTextInsertion {
        fn fails() -> Self {
            Self {
                inserted: std::cell::RefCell::new(Vec::new()),
                fails: true,
            }
        }
    }

    impl TextInsertion for FakeTextInsertion {
        fn insert(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<TextInsertionOutcome, TextInsertionError> {
            self.inserted.borrow_mut().push(transcription.text.clone());
            if self.fails {
                Err(TextInsertionError::new("fake insertion failure"))
            } else {
                Ok(TextInsertionOutcome::ClipboardFree)
            }
        }
    }

    #[derive(Default)]
    struct FakeInsertionRescue {
        rescued: std::cell::RefCell<Vec<String>>,
    }

    impl InsertionRescue for FakeInsertionRescue {
        fn rescue(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
            self.rescued.borrow_mut().push(transcription.text.clone());
            Ok(InsertionRescueOutcome::CopiedToClipboardAndNotified)
        }
    }
}
