//! The Local Diagnostic Log domain (ADR-0019, CONTEXT.md).
//!
//! Redaction-safe development troubleshooting output: by construction these
//! records carry no captured audio samples or transcription text — only the
//! failure surface, error kinds, and non-identifying sizes — so the log can
//! never become hidden Dictation History. [`DiagnosticSink`] is the
//! test/injection seam and [`LocalDiagnosticLog`] stays generic over it.

use crate::{
    AsrError, AsrRuntime, AudioCaptureError, CapturedAudio, DictationEvent, FinalTranscription,
    InsertionRescue, InsertionRescueError, InsertionRescueOutcome, ReadinessItem, RoutingDiagnostics,
    TextInsertion, TextInsertionError, TextInsertionOutcome,
};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// How the Second Opinion router decided one dictation: which engine's
    /// transcript was inserted, which rule escalated (if any), and how long the
    /// whole routed dictation took.
    ///
    /// This is the one event that answers "why did it choose that?", and it can
    /// do so safely because [`RoutingDiagnostics`] is a closed set of enums and
    /// a duration — there is no field on it that could hold what the user said
    /// (ADR-0019).
    RoutingDecision { routing: RoutingDiagnostics },
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

    /// Record how the Second Opinion router decided a dictation. Takes the
    /// already-reduced [`RoutingDiagnostics`] rather than the routed result, so
    /// the transcript is not even in scope at the call site.
    pub fn routing_decision(routing: RoutingDiagnostics) -> Self {
        Self::RoutingDecision { routing }
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
        DiagnosticEvent::RoutingDecision { routing } => {
            // Rendered from the reason codes alone. `escalation: none` is the
            // normal, healthy dictation and is worth logging explicitly: it is
            // the evidence that the second engine stayed asleep.
            let escalation = match routing.escalation {
                Some(reason) => format!("{reason:?}"),
                None => "none".to_string(),
            };
            let second = match routing.second_opinion_engine {
                Some(engine) => engine.id(),
                None => "none",
            };
            format!(
                "asr: routed via {} (escalation: {escalation}, second opinion: {second}, \
                 selection: {:?}, {} ms)",
                routing.selected_engine.id(),
                routing.selection,
                routing.total_latency_ms,
            )
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

/// A [`DiagnosticSink`] that appends rendered lines to a file, or stays a no-op
/// when no path is available (e.g. the app config directory could not be
/// resolved).
pub struct FileDiagnosticSink {
    path: Option<PathBuf>,
}

impl FileDiagnosticSink {
    pub fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn unavailable() -> Self {
        Self { path: None }
    }
}

impl DiagnosticSink for FileDiagnosticSink {
    fn write_line(&mut self, line: &str) {
        let Some(path) = &self.path else {
            return;
        };

        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("could not create diagnostic log directory: {error}");
                return;
            }
        }

        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => file,
            Err(error) => {
                eprintln!("could not open diagnostic log: {error}");
                return;
            }
        };

        if let Err(error) = writeln!(file, "{line}") {
            eprintln!("could not write diagnostic log line: {error}");
        }
    }
}

/// A cheaply cloneable handle to a [`LocalDiagnosticLog`], shared across the
/// diagnostic decorator adapters below so they can all record to the same log.
pub struct SharedDiagnosticLog<S> {
    inner: Arc<Mutex<LocalDiagnosticLog<S>>>,
}

impl<S> Clone for SharedDiagnosticLog<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S> SharedDiagnosticLog<S>
where
    S: DiagnosticSink,
{
    pub fn new(enabled: bool, sink: S) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LocalDiagnosticLog::new(enabled, sink))),
        }
    }

    pub fn record(&self, event: DiagnosticEvent) {
        match self.inner.lock() {
            Ok(mut log) => log.record(event),
            Err(_) => eprintln!("diagnostic log mutex poisoned"),
        }
    }
}

/// Decorates an [`AsrRuntime`], logging transcription outcomes (never the
/// transcript text itself, per ADR-0019) without changing its behavior.
pub struct DiagnosticAsrRuntime<'a, S> {
    runtime: &'a dyn AsrRuntime,
    log: SharedDiagnosticLog<S>,
}

impl<'a, S> DiagnosticAsrRuntime<'a, S> {
    pub fn new(runtime: &'a dyn AsrRuntime, log: SharedDiagnosticLog<S>) -> Self {
        Self { runtime, log }
    }
}

impl<S> AsrRuntime for DiagnosticAsrRuntime<'_, S>
where
    S: DiagnosticSink,
{
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        let result = self.runtime.transcribe(audio);
        match &result {
            Ok(transcription) => self
                .log
                .record(DiagnosticEvent::transcription_completed(transcription)),
            Err(error) => self
                .log
                .record(DiagnosticEvent::transcription_failed(error)),
        }
        result
    }
}

/// Decorates a [`TextInsertion`], logging insertion failures without changing
/// its behavior.
pub struct DiagnosticTextInsertion<'a, S> {
    insertion: &'a dyn TextInsertion,
    log: SharedDiagnosticLog<S>,
}

impl<'a, S> DiagnosticTextInsertion<'a, S> {
    pub fn new(insertion: &'a dyn TextInsertion, log: SharedDiagnosticLog<S>) -> Self {
        Self { insertion, log }
    }
}

impl<S> TextInsertion for DiagnosticTextInsertion<'_, S>
where
    S: DiagnosticSink,
{
    fn insert(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<TextInsertionOutcome, TextInsertionError> {
        let result = self.insertion.insert(transcription);
        if let Err(error) = &result {
            self.log.record(DiagnosticEvent::insertion_failed(error));
        }
        result
    }
}

/// Decorates an [`InsertionRescue`], logging successful rescues without
/// changing its behavior.
pub struct DiagnosticInsertionRescue<'a, S> {
    rescue: &'a dyn InsertionRescue,
    log: SharedDiagnosticLog<S>,
}

impl<'a, S> DiagnosticInsertionRescue<'a, S> {
    pub fn new(rescue: &'a dyn InsertionRescue, log: SharedDiagnosticLog<S>) -> Self {
        Self { rescue, log }
    }
}

impl<S> InsertionRescue for DiagnosticInsertionRescue<'_, S>
where
    S: DiagnosticSink,
{
    fn rescue(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
        let result = self.rescue.rescue(transcription);
        if result.is_ok() {
            self.log.record(DiagnosticEvent::insertion_rescued());
        }
        result
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
    fn routing_decision_log_explains_the_choice_without_the_transcript() {
        // The Second Opinion router's whole promise is that a maintainer can
        // ask "why did it pick that one?" and get an answer. This is the line
        // that answers it, and it must answer with reason codes only.
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::routing_decision(
            crate::RoutingDiagnostics {
                selected_engine: crate::TranscriptionEngine::Parakeet,
                escalation: Some(crate::EscalationReason::EmptyTranscript),
                selection: crate::SelectionReason::SecondOpinionSelected,
                second_opinion_engine: Some(crate::TranscriptionEngine::Parakeet),
                total_latency_ms: 812,
            },
        ));

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("parakeet"), "got: {}", lines[0]);
        assert!(lines[0].contains("EmptyTranscript"), "got: {}", lines[0]);
        assert!(
            lines[0].contains("SecondOpinionSelected"),
            "got: {}",
            lines[0]
        );
        assert!(lines[0].contains("812 ms"), "got: {}", lines[0]);
    }

    #[test]
    fn a_healthy_dictation_is_logged_as_having_woken_no_second_engine() {
        let mut lines: Vec<String> = Vec::new();
        let mut log = LocalDiagnosticLog::new(true, |line: &str| lines.push(line.to_string()));

        log.record(DiagnosticEvent::routing_decision(
            crate::RoutingDiagnostics {
                selected_engine: crate::TranscriptionEngine::Whisper,
                escalation: None,
                selection: crate::SelectionReason::PrimaryAccepted,
                second_opinion_engine: None,
                total_latency_ms: 244,
            },
        ));

        assert!(lines[0].contains("escalation: none"), "got: {}", lines[0]);
        assert!(
            lines[0].contains("second opinion: none"),
            "got: {}",
            lines[0]
        );
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

    #[test]
    fn disabled_file_backed_diagnostic_log_does_not_create_a_log_file() {
        let log_dir = unique_test_dir("diagnostic-log-disabled");
        let log_path = log_dir.join("diagnostics.log");
        let mut log = LocalDiagnosticLog::new(false, FileDiagnosticSink::new(log_path.clone()));

        log.record(DiagnosticEvent::hotkey_transition(DictationEvent::Start));

        assert!(!log_path.exists());
        std::fs::remove_dir_all(&log_dir).ok();
    }

    #[test]
    fn enabled_file_backed_diagnostic_log_appends_redacted_lines() {
        let log_dir = unique_test_dir("diagnostic-log-enabled");
        let log_path = log_dir.join("diagnostics.log");
        let secret = "never write this transcript";
        let mut log = LocalDiagnosticLog::new(true, FileDiagnosticSink::new(log_path.clone()));

        log.record(DiagnosticEvent::hotkey_transition(DictationEvent::Start));
        log.record(DiagnosticEvent::transcription_completed(
            &FinalTranscription {
                text: secret.to_string(),
            },
        ));

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("hotkey"));
        assert!(contents.contains("asr"));
        assert!(!contents.contains(secret));
        assert_eq!(contents.lines().count(), 2);

        std::fs::remove_dir_all(&log_dir).ok();
    }

    #[test]
    fn diagnostic_wrappers_record_asr_insertion_and_rescue_without_transcript_text() {
        let sink = TestDiagnosticSink::default();
        let log = SharedDiagnosticLog::new(true, sink.clone());
        let secret = "do not log these dictated words";
        let runtime = FakeAsrRuntime {
            result: Ok(FinalTranscription {
                text: secret.to_string(),
            }),
        };
        let runtime = DiagnosticAsrRuntime::new(&runtime, log.clone());
        let insertion = FailingTextInsertion;
        let insertion = DiagnosticTextInsertion::new(&insertion, log.clone());
        let rescue = SuccessfulInsertionRescue;
        let rescue = DiagnosticInsertionRescue::new(&rescue, log);

        let transcription =
            AsrRuntime::transcribe(&runtime, CapturedAudio::mono_16khz(vec![0.0])).unwrap();
        let _ = TextInsertion::insert(&insertion, &transcription);
        InsertionRescue::rescue(&rescue, &transcription).unwrap();

        let lines = sink.lines();
        assert!(lines.iter().any(|line| line.contains("asr")));
        assert!(lines.iter().any(|line| line.contains("insertion: failed")));
        assert!(lines.iter().any(|line| line.contains("rescued")));
        assert!(lines.iter().all(|line| !line.contains(secret)));
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-diagnostics-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[derive(Clone, Default)]
    struct TestDiagnosticSink {
        lines: Arc<Mutex<Vec<String>>>,
    }

    impl TestDiagnosticSink {
        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }
    }

    impl DiagnosticSink for TestDiagnosticSink {
        fn write_line(&mut self, line: &str) {
            self.lines.lock().unwrap().push(line.to_string());
        }
    }

    struct FakeAsrRuntime {
        result: Result<FinalTranscription, AsrError>,
    }

    impl AsrRuntime for FakeAsrRuntime {
        fn transcribe(&self, _audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
            self.result.clone()
        }
    }

    struct FailingTextInsertion;

    impl TextInsertion for FailingTextInsertion {
        fn insert(
            &self,
            _transcription: &FinalTranscription,
        ) -> Result<TextInsertionOutcome, TextInsertionError> {
            Err(TextInsertionError::new("test insertion failure"))
        }
    }

    struct SuccessfulInsertionRescue;

    impl InsertionRescue for SuccessfulInsertionRescue {
        fn rescue(
            &self,
            _transcription: &FinalTranscription,
        ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
            Ok(InsertionRescueOutcome::CopiedToClipboardAndNotified)
        }
    }
}
