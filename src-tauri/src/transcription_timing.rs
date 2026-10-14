use crate::{RoutingDiagnostics, TranscriptionEngine};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptionTiming {
    pub engine: TranscriptionEngine,
    pub model_id: String,
    pub audio_ms: u64,
    pub latency_ms: u64,
    pub characters: usize,
}

impl TranscriptionTiming {
    pub fn from_routing(routing: &RoutingDiagnostics) -> Self {
        Self {
            engine: routing.selected_engine,
            model_id: routing.model_id.clone(),
            audio_ms: routing.audio_ms,
            latency_ms: routing.total_latency_ms,
            characters: routing.characters,
        }
    }
}

pub fn render_transcription_timing(timing: &TranscriptionTiming) -> String {
    serde_json::to_string(timing).unwrap_or_else(|error| {
        eprintln!("could not render transcription timing: {error}");
        String::new()
    })
}

pub trait TranscriptionTimingSink {
    fn write_line(&mut self, line: &str);
}

impl<F> TranscriptionTimingSink for F
where
    F: FnMut(&str),
{
    fn write_line(&mut self, line: &str) {
        self(line);
    }
}

pub struct BoxedTranscriptionTimingSink {
    sink: Box<dyn TranscriptionTimingSink + Send + Sync>,
}

impl BoxedTranscriptionTimingSink {
    pub fn new<S>(sink: S) -> Self
    where
        S: TranscriptionTimingSink + Send + Sync + 'static,
    {
        Self {
            sink: Box::new(sink),
        }
    }
}

impl TranscriptionTimingSink for BoxedTranscriptionTimingSink {
    fn write_line(&mut self, line: &str) {
        self.sink.write_line(line);
    }
}

pub type ErasedTranscriptionTimingLog = SharedTranscriptionTimingLog<BoxedTranscriptionTimingSink>;

pub struct FileTranscriptionTimingSink {
    path: Option<PathBuf>,
}

impl FileTranscriptionTimingSink {
    pub fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn unavailable() -> Self {
        Self { path: None }
    }
}

impl TranscriptionTimingSink for FileTranscriptionTimingSink {
    fn write_line(&mut self, line: &str) {
        let Some(path) = &self.path else {
            return;
        };

        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("could not create transcription timing directory: {error}");
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
                eprintln!("could not open transcription timing log: {error}");
                return;
            }
        };

        if let Err(error) = writeln!(file, "{line}") {
            eprintln!("could not write transcription timing line: {error}");
        }
    }
}

pub struct SharedTranscriptionTimingLog<S> {
    inner: Arc<Mutex<S>>,
}

impl<S> Clone for SharedTranscriptionTimingLog<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S> SharedTranscriptionTimingLog<S>
where
    S: TranscriptionTimingSink,
{
    pub fn new(sink: S) -> Self {
        Self {
            inner: Arc::new(Mutex::new(sink)),
        }
    }

    pub fn record(&self, timing: &TranscriptionTiming) {
        let line = render_transcription_timing(timing);
        if !line.is_empty() {
            self.append_line(&line);
        }
    }

    pub fn erased(&self) -> ErasedTranscriptionTimingLog
    where
        S: TranscriptionTimingSink + Send + Sync + 'static,
    {
        ErasedTranscriptionTimingLog::new(BoxedTranscriptionTimingSink::new(LogAsSink {
            log: self.clone(),
        }))
    }

    fn append_line(&self, line: &str) {
        match self.inner.lock() {
            Ok(mut sink) => sink.write_line(line),
            Err(_) => eprintln!("transcription timing log mutex poisoned"),
        }
    }
}

struct LogAsSink<S> {
    log: SharedTranscriptionTimingLog<S>,
}

impl<S> TranscriptionTimingSink for LogAsSink<S>
where
    S: TranscriptionTimingSink + Send + Sync + 'static,
{
    fn write_line(&mut self, line: &str) {
        self.log.append_line(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_line_carries_engine_model_audio_latency_and_characters_without_text() {
        let secret = "the launch codes are four eight one five";
        let timing = TranscriptionTiming {
            engine: TranscriptionEngine::Whisper,
            model_id: "base.en".to_string(),
            audio_ms: 3000,
            latency_ms: 812,
            characters: secret.chars().count(),
        };

        let line = render_transcription_timing(&timing);

        assert!(line.contains("\"engine\":\"whisper\""), "got: {line}");
        assert!(line.contains("\"model_id\":\"base.en\""), "got: {line}");
        assert!(line.contains("\"audio_ms\":3000"), "got: {line}");
        assert!(line.contains("\"latency_ms\":812"), "got: {line}");
        assert!(
            line.contains(&format!("\"characters\":{}", secret.chars().count())),
            "got: {line}"
        );
        assert!(!line.contains(secret), "timing line leaked text: {line}");
        assert!(
            !line.contains("launch codes"),
            "timing line leaked text: {line}"
        );
    }

    #[test]
    fn timing_record_derives_from_routing_diagnostics() {
        let routing = RoutingDiagnostics {
            selected_engine: TranscriptionEngine::Parakeet,
            model_id: "nvidia/parakeet-tdt-0.6b-v2".to_string(),
            escalation: None,
            selection: crate::SelectionReason::PrimaryAccepted,
            second_opinion_engine: None,
            total_latency_ms: 244,
            audio_ms: 1000,
            characters: 5,
        };

        let timing = TranscriptionTiming::from_routing(&routing);

        assert_eq!(timing.engine, TranscriptionEngine::Parakeet);
        assert_eq!(timing.model_id, "nvidia/parakeet-tdt-0.6b-v2");
        assert_eq!(timing.audio_ms, 1000);
        assert_eq!(timing.latency_ms, 244);
        assert_eq!(timing.characters, 5);
    }

    #[test]
    fn shared_timing_log_appends_one_line_per_record() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&lines);
        let log = SharedTranscriptionTimingLog::new(move |line: &str| {
            captured.lock().unwrap().push(line.to_string())
        });
        let timing = TranscriptionTiming {
            engine: TranscriptionEngine::Whisper,
            model_id: "base.en".to_string(),
            audio_ms: 1000,
            latency_ms: 100,
            characters: 5,
        };

        log.record(&timing);
        log.record(&timing);

        assert_eq!(lines.lock().unwrap().len(), 2);
    }
}
