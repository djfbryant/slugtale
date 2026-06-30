use crate::{
    transcribe_captured_audio, AsrError, AsrRuntime, CapturedAudio, FinalTranscription,
    InsertionRescue, InsertionRescueError, TextInsertion,
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

pub struct DictationWorkflow<'a> {
    runtime: &'a dyn AsrRuntime,
    text_insertion: &'a dyn TextInsertion,
    insertion_rescue: &'a dyn InsertionRescue,
}

impl<'a> DictationWorkflow<'a> {
    pub fn new(
        runtime: &'a dyn AsrRuntime,
        text_insertion: &'a dyn TextInsertion,
        insertion_rescue: &'a dyn InsertionRescue,
    ) -> Self {
        Self {
            runtime,
            text_insertion,
            insertion_rescue,
        }
    }

    pub fn complete(
        &self,
        audio: CapturedAudio,
    ) -> Result<FinalTranscription, DictationWorkflowError> {
        let transcription = transcribe_captured_audio(self.runtime, audio)
            .map_err(DictationWorkflowError::Transcription)?;
        let transcription = clean_final_transcription(transcription);
        if self.text_insertion.insert(&transcription).is_err() {
            self.insertion_rescue
                .rescue(&transcription)
                .map_err(DictationWorkflowError::InsertionRescue)?;
        }
        Ok(transcription)
    }
}

/// Apply deterministic Transcript Cleanup before insertion without rewriting
/// meaning or adding generated text.
pub fn clean_final_transcription(transcription: FinalTranscription) -> FinalTranscription {
    let normalized = transcription
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return FinalTranscription { text: normalized };
    };
    let text = if first.is_lowercase() {
        format!("{}{}", first.to_uppercase(), chars.as_str())
    } else {
        normalized
    };

    FinalTranscription { text }
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
        let workflow = DictationWorkflow::new(&runtime, &insertion, &rescue);

        let transcription = workflow
            .complete(CapturedAudio::mono_16khz(vec![0.0, 0.25]))
            .unwrap();

        assert_eq!(transcription.text, "Hello from slugtale");
        assert_eq!(
            insertion.inserted.borrow().as_slice(),
            &["Hello from slugtale"]
        );
    }
    #[test]
    fn dictation_workflow_rescues_cleaned_transcription_when_insertion_fails() {
        let runtime = FakeAsrRuntime::new("  rescue   this transcription ");
        let insertion = FakeTextInsertion::fails();
        let rescue = FakeInsertionRescue::default();
        let workflow = DictationWorkflow::new(&runtime, &insertion, &rescue);

        let transcription = workflow
            .complete(CapturedAudio::mono_16khz(vec![0.0]))
            .unwrap();

        assert_eq!(transcription.text, "Rescue this transcription");
        assert_eq!(
            rescue.rescued.borrow().as_slice(),
            &["Rescue this transcription"]
        );
    }
    #[test]
    fn clean_final_transcription_trims_and_normalizes_repeated_spaces() {
        let transcription = clean_final_transcription(FinalTranscription {
            text: "  hello   from    slugtale  ".to_string(),
        });

        assert_eq!(transcription.text, "Hello from slugtale");
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
            let transcription = clean_final_transcription(FinalTranscription {
                text: input.to_string(),
            });

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
            Ok(FinalTranscription {
                text: self.text.to_string(),
            })
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
