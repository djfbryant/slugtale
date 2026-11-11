use crate::{RoutingDiagnostics, TranscriptionEngine};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptionTiming {
    pub engine: TranscriptionEngine,
    pub model_id: String,
    pub audio_ms: u64,
    pub latency_ms: u64,
    pub characters: usize,
    pub primary_engine: TranscriptionEngine,
    pub primary_ms: u64,
    pub escalation: Option<crate::EscalationReason>,
    pub second_opinion_engine: Option<TranscriptionEngine>,
    pub second_opinion_ms: Option<u64>,
    pub selection: crate::SelectionReason,
}

impl TranscriptionTiming {
    pub fn from_routing(routing: &RoutingDiagnostics) -> Self {
        Self {
            engine: routing.selected_engine,
            model_id: routing.model_id.clone(),
            audio_ms: routing.audio_ms,
            latency_ms: routing.total_latency_ms,
            characters: routing.characters,
            primary_engine: routing.primary_engine,
            primary_ms: routing.primary_latency_ms,
            escalation: routing.escalation,
            second_opinion_engine: routing.second_opinion_engine,
            second_opinion_ms: routing.second_opinion_latency_ms,
            selection: routing.selection,
        }
    }
}

/// A Stage Timing: how long each named step of one app task took, such as
/// app startup, an engine warm-up, or one Dictation Segment. Stage names and
/// notes are fixed labels and technical values only; never transcription
/// text or audio (ADR-0028).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageTiming {
    pub event: &'static str,
    pub at_ms: u64,
    pub total_ms: u64,
    pub stages: Vec<(&'static str, u64)>,
    pub notes: Vec<(&'static str, String)>,
}

impl StageTiming {
    pub fn stage_ms(&self, stage: &str) -> Option<u64> {
        self.stages
            .iter()
            .find(|(name, _)| *name == stage)
            .map(|(_, ms)| *ms)
    }

    pub fn note(&self, key: &str) -> Option<&str> {
        self.notes
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_str())
    }
}

impl Serialize for StageTiming {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        struct OrderedStages<'a>(&'a [(&'static str, u64)]);
        impl Serialize for OrderedStages<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for (name, ms) in self.0 {
                    map.serialize_entry(name, ms)?;
                }
                map.end()
            }
        }

        let mut map = serializer.serialize_map(Some(4 + self.notes.len()))?;
        map.serialize_entry("event", self.event)?;
        map.serialize_entry("at_ms", &self.at_ms)?;
        map.serialize_entry("total_ms", &self.total_ms)?;
        map.serialize_entry("stages", &OrderedStages(&self.stages))?;
        for (key, value) in &self.notes {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Measures the steps of one task in order. Each [`StageTimer::mark`] closes
/// the stage that began at the previous mark. The total is the wall-clock time
/// to [`StageTimer::finish`], so the stages sum to the total minus the
/// unmarked tail between the last mark and `finish`.
pub struct StageTimer {
    event: &'static str,
    at: SystemTime,
    started: Instant,
    last: Instant,
    stages: Vec<(&'static str, u64)>,
    notes: Vec<(&'static str, String)>,
}

impl StageTimer {
    pub fn start(event: &'static str) -> Self {
        Self::started_at(event, Instant::now())
    }

    /// Start the timer at an earlier moment, such as when the user pressed
    /// the hotkey, so the first stage covers the wait before this code ran.
    pub fn started_at(event: &'static str, started: Instant) -> Self {
        let now = Instant::now();
        let at = SystemTime::now()
            .checked_sub(now.saturating_duration_since(started))
            .unwrap_or_else(SystemTime::now);
        Self {
            event,
            at,
            started,
            last: started,
            stages: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.stages
            .push((stage, millis(now.saturating_duration_since(self.last))));
        self.last = now;
    }

    pub fn note(&mut self, key: &'static str, value: impl Into<String>) {
        self.notes.push((key, value.into()));
    }

    pub fn finish(self) -> StageTiming {
        StageTiming {
            event: self.event,
            at_ms: self
                .at
                .duration_since(UNIX_EPOCH)
                .map(millis)
                .unwrap_or_default(),
            total_ms: millis(Instant::now().saturating_duration_since(self.started)),
            stages: self.stages,
            notes: self.notes,
        }
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

pub fn render_transcription_timing(timing: &TranscriptionTiming) -> String {
    #[derive(Serialize)]
    struct Line<'a> {
        event: &'static str,
        #[serde(flatten)]
        timing: &'a TranscriptionTiming,
    }
    render_line(&Line {
        event: "transcription",
        timing,
    })
}

pub fn render_stage_timing(timing: &StageTiming) -> String {
    render_line(timing)
}

fn render_line(line: &impl Serialize) -> String {
    serde_json::to_string(line).unwrap_or_else(|error| {
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

    pub fn record_stages(&self, timing: &StageTiming) {
        let line = render_stage_timing(timing);
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
            primary_engine: TranscriptionEngine::Whisper,
            primary_ms: 812,
            escalation: None,
            second_opinion_engine: None,
            second_opinion_ms: None,
            selection: crate::SelectionReason::PrimaryAccepted,
        };

        let line = render_transcription_timing(&timing);

        assert!(line.contains("\"engine\":\"whisper\""), "got: {line}");
        assert!(line.contains("\"event\":\"transcription\""), "got: {line}");
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
            primary_engine: TranscriptionEngine::Phonon,
            primary_latency_ms: 90,
            second_opinion_latency_ms: Some(150),
        };

        let timing = TranscriptionTiming::from_routing(&routing);

        assert_eq!(timing.engine, TranscriptionEngine::Parakeet);
        assert_eq!(timing.model_id, "nvidia/parakeet-tdt-0.6b-v2");
        assert_eq!(timing.audio_ms, 1000);
        assert_eq!(timing.latency_ms, 244);
        assert_eq!(timing.characters, 5);
        assert_eq!(timing.primary_engine, TranscriptionEngine::Phonon);
        assert_eq!(timing.primary_ms, 90);
        assert_eq!(timing.second_opinion_ms, Some(150));
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
            primary_engine: TranscriptionEngine::Whisper,
            primary_ms: 100,
            escalation: None,
            second_opinion_engine: None,
            second_opinion_ms: None,
            selection: crate::SelectionReason::PrimaryAccepted,
        };

        log.record(&timing);
        log.record(&timing);

        assert_eq!(lines.lock().unwrap().len(), 2);
    }

    #[test]
    fn stage_timing_line_keeps_stages_in_the_order_they_ran() {
        let timing = StageTiming {
            event: "dictation-segment",
            at_ms: 1_700_000_000_000,
            total_ms: 60,
            stages: vec![("transcribe", 50), ("cleanup", 0), ("insert", 10)],
            notes: vec![("engine", "phonon".to_string())],
        };

        let line = render_stage_timing(&timing);

        assert_eq!(
            line,
            "{\"event\":\"dictation-segment\",\"at_ms\":1700000000000,\"total_ms\":60,\
             \"stages\":{\"transcribe\":50,\"cleanup\":0,\"insert\":10},\"engine\":\"phonon\"}"
        );
    }

    #[test]
    fn stage_timer_stages_add_up_to_the_total() {
        let started = Instant::now() - Duration::from_millis(40);
        let mut timer = StageTimer::started_at("app-startup", started);
        timer.mark("waited");
        std::thread::sleep(Duration::from_millis(5));
        timer.mark("worked");
        timer.note("result", "ok");

        let timing = timer.finish();

        assert!(timing.stage_ms("waited").unwrap() >= 40);
        assert!(timing.stage_ms("worked").unwrap() >= 5);
        let sum: u64 = timing.stages.iter().map(|(_, ms)| ms).sum();
        assert!(timing.total_ms.abs_diff(sum) <= 1, "{timing:?}");
        assert_eq!(timing.note("result"), Some("ok"));
    }

    #[test]
    fn shared_timing_log_appends_stage_timings_beside_transcriptions() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&lines);
        let log = SharedTranscriptionTimingLog::new(move |line: &str| {
            captured.lock().unwrap().push(line.to_string())
        });

        log.record_stages(&StageTimer::start("engine-warm-up").finish());

        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("\"event\":\"engine-warm-up\""),
            "got: {}",
            lines[0]
        );
    }
}
