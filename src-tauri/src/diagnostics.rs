//! The Local Diagnostic Log domain (ADR-0019, CONTEXT.md).
//!
//! Redaction-safe development troubleshooting output: by construction these
//! records carry no captured audio samples or transcription text — only the
//! failure surface, error kinds, and non-identifying sizes — so the log can
//! never become hidden Dictation History. [`DiagnosticSink`] is the
//! test/injection seam and [`LocalDiagnosticLog`] stays generic over it.

use crate::{
    AsrError, AudioCaptureError, DictationEvent, FinalTranscription, ReadinessItem,
    TextInsertionError,
};

/// A redaction-safe record for the Local Diagnostic Log (ADR-0019, CONTEXT.md).
/// By construction it carries no captured audio samples or transcription text —
/// only the failure surface, error kinds, and non-identifying sizes — so the log
/// can never become hidden Dictation History.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticEvent {
    /// A Dictation Readiness check did not reach ready; carries the unmet item ids.
    ReadinessIncomplete { missing: Vec<String> },
    /// A Final Transcription completed; carries only its character count, never the
    /// transcript text (ADR-0019).
    TranscriptionCompleted { characters: usize },
    /// Local transcription failed; carries the technical error description only.
    TranscriptionFailed { reason: String },
    /// Audio capture failed; carries the technical error description, never samples.
    AudioCaptureFailed { reason: String },
    /// A dictation lifecycle transition driven by the hotkey (Start/Stop/Cancel),
    /// useful for tracing where the dictation pipeline stalls.
    HotkeyTransition { event: DictationEvent },
    /// Text Insertion failed; carries the technical error description only.
    InsertionFailed { reason: String },
    /// Insertion Rescue ran after insertion failed, preserving the transcription
    /// to the clipboard. Carries no transcript text (ADR-0019).
    InsertionRescued,
}

impl DiagnosticEvent {
    /// Reduce unmet [`ReadinessItem`]s to their ids for logging.
    pub fn readiness_incomplete(missing: &[ReadinessItem]) -> Self {
        Self::ReadinessIncomplete {
            missing: missing.iter().map(|item| item.id.clone()).collect(),
        }
    }

    /// Reduce a [`FinalTranscription`] to a character count so the transcript text
    /// can never reach the Local Diagnostic Log.
    pub fn transcription_completed(transcription: &FinalTranscription) -> Self {
        Self::TranscriptionCompleted {
            characters: transcription.text.chars().count(),
        }
    }

    /// Record an [`AsrError`] by its technical description; AsrError never carries
    /// transcript text.
    pub fn transcription_failed(error: &AsrError) -> Self {
        Self::TranscriptionFailed {
            reason: error.to_string(),
        }
    }

    /// Record an [`AudioCaptureError`] by its technical description; captured audio
    /// samples are never carried.
    pub fn audio_capture_failed(error: &AudioCaptureError) -> Self {
        Self::AudioCaptureFailed {
            reason: error.to_string(),
        }
    }

    /// Record a hotkey-driven dictation lifecycle transition.
    pub fn hotkey_transition(event: DictationEvent) -> Self {
        Self::HotkeyTransition { event }
    }

    /// Record a [`TextInsertionError`] by its technical description; the
    /// transcription being inserted is never carried.
    pub fn insertion_failed(error: &TextInsertionError) -> Self {
        Self::InsertionFailed {
            reason: error.to_string(),
        }
    }

    /// Record that Insertion Rescue preserved a transcription after insertion
    /// failed.
    pub fn insertion_rescued() -> Self {
        Self::InsertionRescued
    }
}

/// Format one [`DiagnosticEvent`] as a single redacted Local Diagnostic Log line.
pub fn render_diagnostic_event(event: &DiagnosticEvent) -> String {
    match event {
        DiagnosticEvent::ReadinessIncomplete { missing } => {
            format!("readiness: not ready (missing: {})", missing.join(", "))
        }
        DiagnosticEvent::TranscriptionCompleted { characters } => {
            format!("asr: final transcription completed ({characters} chars)")
        }
        DiagnosticEvent::TranscriptionFailed { reason } => {
            format!("asr: transcription failed ({reason})")
        }
        DiagnosticEvent::AudioCaptureFailed { reason } => {
            format!("audio: capture failed ({reason})")
        }
        DiagnosticEvent::HotkeyTransition { event } => {
            format!("hotkey: dictation {event:?}")
        }
        DiagnosticEvent::InsertionFailed { reason } => {
            format!("insertion: failed ({reason})")
        }
        DiagnosticEvent::InsertionRescued => {
            "insertion: rescued transcription to clipboard".to_string()
        }
    }
}

/// Destination for rendered Local Diagnostic Log lines. An `FnMut(&str)` works as
/// a sink, matching the [`DictationEventSink`](crate::DictationEventSink) idiom.
pub trait DiagnosticSink {
    fn write_line(&mut self, line: &str);
}

impl<F> DiagnosticSink for F
where
    F: FnMut(&str),
{
    fn write_line(&mut self, line: &str) {
        self(line);
    }
}

/// The Local Diagnostic Log (ADR-0019): development troubleshooting output gated
/// by the user's `diagnostic_logging` preference (off by default). When disabled
/// it records nothing, so no log file accumulates unless the user opts in.
pub struct LocalDiagnosticLog<S> {
    enabled: bool,
    sink: S,
}

impl<S> LocalDiagnosticLog<S>
where
    S: DiagnosticSink,
{
    pub fn new(enabled: bool, sink: S) -> Self {
        Self { enabled, sink }
    }

    pub fn record(&mut self, event: DiagnosticEvent) {
        if self.enabled {
            self.sink.write_line(&render_diagnostic_event(&event));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;

    #[test]
    fn enabled_diagnostic_log_records_a_readiness_failure_line() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::readiness_incomplete(&[
            ReadinessItem::missing("microphone", "Microphone access", true),
        ]));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("readiness"));
        assert!(lines[0].contains("microphone"));
    }

    #[test]
    fn disabled_diagnostic_log_records_nothing() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(false, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::readiness_incomplete(&[
            ReadinessItem::missing("microphone", "Microphone access", true),
        ]));

        assert!(lines.is_empty());
    }

    #[test]
    fn diagnostic_logging_is_off_by_default() {
        assert!(!Settings::default().diagnostic_logging);
    }

    #[test]
    fn transcription_completed_log_never_includes_the_transcript_text() {
        let secret = "the launch codes are four eight one five";
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::transcription_completed(
            &FinalTranscription {
                text: secret.to_string(),
            },
        ));

        assert_eq!(lines.len(), 1);
        assert!(
            !lines[0].contains(secret),
            "diagnostic log leaked transcription text: {}",
            lines[0]
        );
        assert!(!lines[0].contains("launch codes"));
        // A character count is a safe, non-identifying size.
        assert!(lines[0].contains(&secret.chars().count().to_string()));
    }

    #[test]
    fn transcription_failure_log_records_the_asr_error_surface() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::transcription_failed(
            &AsrError::ModelMissing {
                path: std::path::PathBuf::from("/models/base.bin"),
            },
        ));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("asr"));
        assert!(lines[0].contains("/models/base.bin"));
    }

    #[test]
    fn audio_capture_failure_log_records_the_audio_surface() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::audio_capture_failed(
            &AudioCaptureError::new("no input device"),
        ));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("audio"));
        assert!(lines[0].contains("no input device"));
    }

    #[test]
    fn hotkey_transition_log_records_the_hotkey_surface() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::hotkey_transition(DictationEvent::Start));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hotkey"));
        assert!(lines[0].to_lowercase().contains("start"));
    }

    #[test]
    fn insertion_failure_log_records_the_insertion_surface() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::insertion_failed(&TextInsertionError::new(
            "accessibility permission denied",
        )));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("insertion"));
        assert!(lines[0].contains("accessibility permission denied"));
    }

    #[test]
    fn insertion_rescue_log_records_the_rescue_without_transcript_text() {
        let secret = "remember the milk";
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        // Insertion Rescue happens after a Final Transcription exists, but the log
        // line must never carry the rescued text.
        log.record(DiagnosticEvent::insertion_rescued());

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("insertion"));
        assert!(lines[0].to_lowercase().contains("rescue"));
        assert!(!lines[0].contains(secret));
    }

    #[test]
    fn transcription_completed_log_reduces_any_transcript_to_a_character_count() {
        // The only constructor that ingests dictation content is
        // transcription_completed; whatever the transcript's shape, the log keeps
        // only its length, never the words (ADR-0019).
        let secrets = [
            "the eagle lands at midnight",
            "  leading and trailing space  ",
            "café déjà vu naïve",
            "line one\nline two",
        ];

        for secret in secrets {
            let event = DiagnosticEvent::transcription_completed(&FinalTranscription {
                text: secret.to_string(),
            });
            let line = render_diagnostic_event(&event);

            assert!(!line.contains(secret), "leaked transcript text: {line}");
            assert!(line.contains(&secret.chars().count().to_string()));
        }
    }
}
