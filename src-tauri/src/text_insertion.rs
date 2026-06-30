//! The Text Insertion and Insertion Rescue domains (CONTEXT.md).
//!
//! [`TextInsertionPipeline`] is the primary path: it tries a clipboard-free
//! insertion first and falls back to a clipboard paste. When insertion fails
//! entirely, [`ClipboardInsertionRescue`] preserves the transcription by
//! copying it to the clipboard and notifying the user. The `*System` traits are
//! the platform-adapter seam (the OS adapters live in the `macos` module), so
//! the pipeline and rescue stay fully unit-testable without a running runtime.

use crate::FinalTranscription;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInsertionError {
    message: String,
}

impl TextInsertionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TextInsertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "text insertion failed: {}", self.message)
    }
}

impl std::error::Error for TextInsertionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInsertionOutcome {
    ClipboardFree,
    ClipboardFallback,
}

pub trait TextInsertion {
    fn insert(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<TextInsertionOutcome, TextInsertionError>;
}

pub trait TextInsertionSystem {
    fn insert_clipboard_free(&self, text: &str) -> Result<(), TextInsertionError>;
    fn insert_from_clipboard(&self, text: &str) -> Result<(), TextInsertionError>;
}

pub struct TextInsertionPipeline<S> {
    system: S,
}

impl<S> TextInsertionPipeline<S>
where
    S: TextInsertionSystem,
{
    pub fn new(system: S) -> Self {
        Self { system }
    }

    pub fn system(&self) -> &S {
        &self.system
    }
}

impl<S> TextInsertion for TextInsertionPipeline<S>
where
    S: TextInsertionSystem,
{
    fn insert(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<TextInsertionOutcome, TextInsertionError> {
        if self
            .system
            .insert_clipboard_free(&transcription.text)
            .is_ok()
        {
            return Ok(TextInsertionOutcome::ClipboardFree);
        }

        self.system.insert_from_clipboard(&transcription.text)?;
        Ok(TextInsertionOutcome::ClipboardFallback)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionRescueError {
    message: String,
}

impl InsertionRescueError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for InsertionRescueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "insertion rescue failed: {}", self.message)
    }
}

impl std::error::Error for InsertionRescueError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionRescueOutcome {
    CopiedToClipboardAndNotified,
}

pub trait InsertionRescue {
    fn rescue(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<InsertionRescueOutcome, InsertionRescueError>;
}

pub trait InsertionRescueSystem {
    fn copy_to_clipboard(&self, text: &str) -> Result<(), InsertionRescueError>;
    fn notify_user(&self, title: &str, body: &str) -> Result<(), InsertionRescueError>;
}

pub struct ClipboardInsertionRescue<S> {
    system: S,
}

impl<S> ClipboardInsertionRescue<S>
where
    S: InsertionRescueSystem,
{
    pub fn new(system: S) -> Self {
        Self { system }
    }

    pub fn system(&self) -> &S {
        &self.system
    }
}

impl<S> InsertionRescue for ClipboardInsertionRescue<S>
where
    S: InsertionRescueSystem,
{
    fn rescue(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
        self.system.copy_to_clipboard(&transcription.text)?;
        self.system.notify_user(
            "Text insertion failed",
            "Your transcription was copied to the clipboard.",
        )?;
        Ok(InsertionRescueOutcome::CopiedToClipboardAndNotified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FinalTranscription;

    #[test]
    fn text_insertion_uses_clipboard_free_path_before_clipboard_fallback() {
        let system = FakeTextInsertionSystem::default();
        let insertion = TextInsertionPipeline::new(system);

        let outcome = insertion
            .insert(&FinalTranscription {
                text: "Inserted without clipboard".to_string(),
            })
            .unwrap();

        assert_eq!(outcome, TextInsertionOutcome::ClipboardFree);
        assert_eq!(
            insertion.system().events.borrow().as_slice(),
            &["clipboard_free:Inserted without clipboard"]
        );
    }

    #[test]
    fn text_insertion_falls_back_to_clipboard_when_clipboard_free_path_fails() {
        let system = FakeTextInsertionSystem {
            clipboard_free_fails: true,
            ..FakeTextInsertionSystem::default()
        };
        let insertion = TextInsertionPipeline::new(system);

        let outcome = insertion
            .insert(&FinalTranscription {
                text: "Inserted through fallback".to_string(),
            })
            .unwrap();

        assert_eq!(outcome, TextInsertionOutcome::ClipboardFallback);
        assert_eq!(
            insertion.system().events.borrow().as_slice(),
            &[
                "clipboard_free:Inserted through fallback",
                "clipboard_fallback:Inserted through fallback"
            ]
        );
    }

    #[test]
    fn insertion_rescue_copies_transcription_to_clipboard_and_notifies_user() {
        let system = FakeInsertionRescueSystem::default();
        let rescue = ClipboardInsertionRescue::new(system);

        let outcome = rescue
            .rescue(&FinalTranscription {
                text: "Preserve this transcription".to_string(),
            })
            .unwrap();

        assert_eq!(
            outcome,
            InsertionRescueOutcome::CopiedToClipboardAndNotified
        );
        assert_eq!(
            rescue.system().events.borrow().as_slice(),
            &[
                "copy_to_clipboard:Preserve this transcription",
                "notify:Text insertion failed:Your transcription was copied to the clipboard."
            ]
        );
    }

    #[derive(Default)]
    struct FakeTextInsertionSystem {
        events: std::cell::RefCell<Vec<String>>,
        clipboard_free_fails: bool,
        clipboard_fallback_fails: bool,
    }

    impl TextInsertionSystem for FakeTextInsertionSystem {
        fn insert_clipboard_free(&self, text: &str) -> Result<(), TextInsertionError> {
            self.events
                .borrow_mut()
                .push(format!("clipboard_free:{text}"));
            if self.clipboard_free_fails {
                Err(TextInsertionError::new("fake clipboard-free failure"))
            } else {
                Ok(())
            }
        }

        fn insert_from_clipboard(&self, text: &str) -> Result<(), TextInsertionError> {
            self.events
                .borrow_mut()
                .push(format!("clipboard_fallback:{text}"));
            if self.clipboard_fallback_fails {
                Err(TextInsertionError::new("fake clipboard fallback failure"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakeInsertionRescueSystem {
        events: std::cell::RefCell<Vec<String>>,
    }

    impl InsertionRescueSystem for FakeInsertionRescueSystem {
        fn copy_to_clipboard(&self, text: &str) -> Result<(), InsertionRescueError> {
            self.events
                .borrow_mut()
                .push(format!("copy_to_clipboard:{text}"));
            Ok(())
        }

        fn notify_user(&self, title: &str, body: &str) -> Result<(), InsertionRescueError> {
            self.events
                .borrow_mut()
                .push(format!("notify:{title}:{body}"));
            Ok(())
        }
    }
}
