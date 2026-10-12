
use crate::{
    captured_audio_duration, second_opinion_transport, AsrError, CapturedAudio, EngineTranscriber,
    EngineTranscription, TranscriptionEngine,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecondOpinionMode {
    Off,
    Automatic,
}

impl Default for SecondOpinionMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationReason {
    EmptyTranscript,
    /// **This rule is inert today.** Neither engine currently shipped reports
    /// confidence: whisper.cpp gives Slugtale plain segment text, and
    /// `parakeet-rs` 0.3.6 discards the joint logits after its argmax, so it
    /// returns tokens and timings but no score. Apple SpeechTranscriber is the
    /// first engine that can trip this. The rule is implemented and tested
    /// anyway because the alternative — adding it later, under pressure, to a
    /// router already in users' hands — is how selection logic goes wrong.
    LowConfidence,
    RepeatedPhrase,
    ImplausiblyShortForDuration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionReason {
    PrimaryAccepted,
    PrimaryKeptAfterSecondOpinion,
    SecondOpinionSelected,
    PrimaryKeptSecondOpinionUnavailable,
}

#[derive(Debug, Clone, PartialEq)]
struct RoutedTranscription {
    selected: EngineTranscription,
    escalation: Option<EscalationReason>,
    selection: SelectionReason,
    second_opinion_engine: Option<TranscriptionEngine>,
    total_latency: Duration,
}

impl RoutedTranscription {
    fn diagnostics(&self) -> RoutingDiagnostics {
        RoutingDiagnostics {
            selected_engine: self.selected.engine,
            escalation: self.escalation,
            selection: self.selection,
            second_opinion_engine: self.second_opinion_engine,
            total_latency_ms: self.total_latency.as_millis() as u64,
        }
    }
}

/// What the Local Diagnostic Log may record about a routed dictation. Contains
/// no transcript, no alternatives, no confidence value, and no audio — only
/// engine identity, reason codes, and a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingDiagnostics {
    pub selected_engine: TranscriptionEngine,
    pub escalation: Option<EscalationReason>,
    pub selection: SelectionReason,
    pub second_opinion_engine: Option<TranscriptionEngine>,
    pub total_latency_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscalationPolicy {
    minimum_confidence: f32,
    pub minimum_words_per_second: f32,
    minimum_judged_duration: Duration,
    repeated_phrase_run: usize,
    pub second_opinion_budget: Duration,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            minimum_confidence: 0.45,
            minimum_words_per_second: 0.4,
            minimum_judged_duration: Duration::from_millis(600),
            repeated_phrase_run: 3,
            second_opinion_budget: Duration::from_secs(6),
        }
    }
}

impl EscalationPolicy {
    #[cfg(test)]
    pub(crate) fn with_budget(mut self, budget: Duration) -> Self {
        self.second_opinion_budget = budget;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_minimum_words_per_second(mut self, words_per_second: f32) -> Self {
        self.minimum_words_per_second = words_per_second;
        self
    }

    fn escalation_for(
        &self,
        result: &EngineTranscription,
        audio_duration: Duration,
    ) -> Option<EscalationReason> {
        let text = result.text().trim();
        let judged = audio_duration >= self.minimum_judged_duration;

        if text.is_empty() {
            return judged.then_some(EscalationReason::EmptyTranscript);
        }

        if has_repeated_phrase(text, self.repeated_phrase_run) {
            return Some(EscalationReason::RepeatedPhrase);
        }

        if judged {
            let words = text.split_whitespace().count() as f32;
            let seconds = audio_duration.as_secs_f32();
            if seconds > 0.0 && words / seconds < self.minimum_words_per_second {
                return Some(EscalationReason::ImplausiblyShortForDuration);
            }
        }

        if let Some(score) = result.confidence.escalation_score() {
            if score < self.minimum_confidence {
                return Some(EscalationReason::LowConfidence);
            }
        }

        None
    }

    /// Choose one complete transcript once both engines have answered.
    ///
    /// The rules only ever compare an engine against an absolute standard —
    /// "is this empty?", "does this loop?", "does this clear its own confidence
    /// threshold?" — and never against the other engine's score, because
    /// confidence from two engines is not on the same scale until it has been
    /// calibrated on the same recordings. Every tie keeps the primary, so the
    /// router is deterministic and a second opinion can only ever fix a result
    /// it can positively explain.
    pub fn select(
        &self,
        _primary: &EngineTranscription,
        second: &EngineTranscription,
        escalation: EscalationReason,
        audio_duration: Duration,
    ) -> SelectionReason {
        let second_is_healthy = self.escalation_for(second, audio_duration).is_none();

        let second_wins = match escalation {
            EscalationReason::EmptyTranscript => !second.text().trim().is_empty(),
            EscalationReason::RepeatedPhrase => {
                !has_repeated_phrase(second.text().trim(), self.repeated_phrase_run)
                    && !second.text().trim().is_empty()
            }
            EscalationReason::ImplausiblyShortForDuration => second_is_healthy,
            EscalationReason::LowConfidence => {
                second_is_healthy && second.confidence.escalation_score().is_some()
            }
        };

        if second_wins {
            SelectionReason::SecondOpinionSelected
        } else {
            SelectionReason::PrimaryKeptAfterSecondOpinion
        }
    }
}

#[derive(Clone, Default)]
pub struct SecondOpinionCoordinator {
    gate: second_opinion_transport::InFlightGate,
}

pub struct SecondOpinionRouter {
    primary: Arc<dyn EngineTranscriber>,
    second: Option<Arc<dyn EngineTranscriber>>,
    mode: SecondOpinionMode,
    policy: EscalationPolicy,
    coordinator: SecondOpinionCoordinator,
    observer: Option<Arc<dyn Fn(RoutingDiagnostics) + Send + Sync>>,
}

impl SecondOpinionRouter {
    pub fn single(primary: Arc<dyn EngineTranscriber>) -> Self {
        Self {
            primary,
            second: None,
            mode: SecondOpinionMode::Off,
            policy: EscalationPolicy::default(),
            coordinator: SecondOpinionCoordinator::default(),
            observer: None,
        }
    }

    pub fn new(
        primary: Arc<dyn EngineTranscriber>,
        second: Arc<dyn EngineTranscriber>,
        mode: SecondOpinionMode,
    ) -> Self {
        Self {
            primary,
            second: Some(second),
            mode,
            policy: EscalationPolicy::default(),
            coordinator: SecondOpinionCoordinator::default(),
            observer: None,
        }
    }

    pub fn with_coordinator(mut self, coordinator: SecondOpinionCoordinator) -> Self {
        self.coordinator = coordinator;
        self
    }

    #[cfg(test)]
    fn with_policy(mut self, policy: EscalationPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn observing(
        mut self,
        observer: impl Fn(RoutingDiagnostics) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    fn route(&self, audio: &CapturedAudio) -> Result<RoutedTranscription, AsrError> {
        let routed = self.routed(audio)?;
        if let Some(observer) = self.observer.as_ref() {
            observer(routed.diagnostics());
        }
        Ok(routed)
    }

    fn routed(&self, audio: &CapturedAudio) -> Result<RoutedTranscription, AsrError> {
        let started = Instant::now();
        let primary = self.primary.transcribe(audio)?;
        let audio_duration = captured_audio_duration(audio);

        let Some(escalation) = self.escalation_for(&primary, audio_duration) else {
            return Ok(RoutedTranscription {
                selected: primary,
                escalation: None,
                selection: SelectionReason::PrimaryAccepted,
                second_opinion_engine: None,
                total_latency: started.elapsed(),
            });
        };

        let Some(second_provider) = self.available_second_opinion() else {
            return Ok(RoutedTranscription {
                selected: primary,
                escalation: Some(escalation),
                selection: SelectionReason::PrimaryKeptSecondOpinionUnavailable,
                second_opinion_engine: None,
                total_latency: started.elapsed(),
            });
        };

        let second_engine = second_provider.engine();
        let unavailable = |latency| RoutedTranscription {
            selected: primary.clone(),
            escalation: Some(escalation),
            selection: SelectionReason::PrimaryKeptSecondOpinionUnavailable,
            second_opinion_engine: Some(second_engine),
            total_latency: latency,
        };

        let Some(second) =
            self.second_opinion_within_budget(second_provider, audio, &self.coordinator.gate)
        else {
            return Ok(unavailable(started.elapsed()));
        };

        let selection = self
            .policy
            .select(&primary, &second, escalation, audio_duration);
        let selected = match selection {
            SelectionReason::SecondOpinionSelected => second,
            _ => primary,
        };

        Ok(RoutedTranscription {
            selected,
            escalation: Some(escalation),
            selection,
            second_opinion_engine: Some(second_engine),
            total_latency: started.elapsed(),
        })
    }

    fn escalation_for(
        &self,
        primary: &EngineTranscription,
        audio_duration: Duration,
    ) -> Option<EscalationReason> {
        if self.mode == SecondOpinionMode::Off || self.second.is_none() {
            return None;
        }
        self.policy.escalation_for(primary, audio_duration)
    }

    fn available_second_opinion(&self) -> Option<Arc<dyn EngineTranscriber>> {
        let second = self.second.as_ref()?;
        second.availability().is_available().then(|| second.clone())
    }

    fn second_opinion_within_budget(
        &self,
        provider: Arc<dyn EngineTranscriber>,
        audio: &CapturedAudio,
        gate: &second_opinion_transport::InFlightGate,
    ) -> Option<EngineTranscription> {
        second_opinion_transport::run_within_budget(
            provider,
            audio,
            gate,
            self.policy.second_opinion_budget,
        )
    }
}

impl crate::AsrRuntime for SecondOpinionRouter {
    fn transcribe(&self, audio: CapturedAudio) -> Result<crate::FinalTranscription, AsrError> {
        Ok(self.route(&audio)?.selected.transcription)
    }
}

fn has_repeated_phrase(text: &str, minimum_run: usize) -> bool {
    if minimum_run < 2 {
        return false;
    }

    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|character| character.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect();

    for size in 1..=4usize {
        if words.len() < size * minimum_run {
            break;
        }
        for start in 0..=words.len() - size * minimum_run {
            let phrase = &words[start..start + size];
            let mut runs = 1;
            let mut at = start + size;
            while at + size <= words.len() && &words[at..at + size] == phrase {
                runs += 1;
                at += size;
            }
            if runs >= minimum_run {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EngineAssetLifecycle, EngineAssets, EngineAvailability, EngineConfidence, EngineMetadata,
        EngineTranscriber, EngineUnavailable, FinalTranscription,
    };

    #[test]
    fn second_opinion_is_off_until_the_benchmark_says_otherwise() {
        assert_eq!(SecondOpinionMode::default(), SecondOpinionMode::Off);
    }

    #[test]
    fn a_healthy_dictation_never_wakes_the_second_engine() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "Hello from Slugtale");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "should not run");
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(2.0)).unwrap();

        assert_eq!(routed.selected.text(), "Hello from Slugtale");
        assert_eq!(routed.escalation, None);
        assert_eq!(routed.selection, SelectionReason::PrimaryAccepted);
        assert_eq!(routed.second_opinion_engine, None);
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn automatic_mode_off_leaves_an_anomalous_result_alone() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "the rescue transcript");
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Off);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(routed.selected.text(), "");
        assert_eq!(routed.escalation, None);
        assert_eq!(routed.selection, SelectionReason::PrimaryAccepted);
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn an_empty_transcript_is_replaced_by_the_second_opinion() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "   ");
        let second = FakeProvider::new(
            TranscriptionEngine::Parakeet,
            "Book the meeting for Tuesday",
        );
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(routed.escalation, Some(EscalationReason::EmptyTranscript));
        assert_eq!(routed.selection, SelectionReason::SecondOpinionSelected);
        assert_eq!(routed.selected.text(), "Book the meeting for Tuesday");
        assert_eq!(routed.selected.engine, TranscriptionEngine::Parakeet);
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn a_brief_recording_is_allowed_to_transcribe_to_nothing() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "invented words");
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(0.3)).unwrap();

        assert_eq!(routed.escalation, None);
        assert_eq!(routed.selected.text(), "");
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn a_looping_transcript_escalates_and_a_clean_second_opinion_wins() {
        let primary = FakeProvider::new(
            TranscriptionEngine::Whisper,
            "I don't know. I don't know. I don't know.",
        );
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "I am not sure about that");
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(4.0)).unwrap();

        assert_eq!(routed.escalation, Some(EscalationReason::RepeatedPhrase));
        assert_eq!(routed.selection, SelectionReason::SecondOpinionSelected);
    }

    #[test]
    fn a_second_opinion_that_loops_too_does_not_replace_the_primary() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "yes yes yes yes");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "no no no no");
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(4.0)).unwrap();

        assert_eq!(routed.escalation, Some(EscalationReason::RepeatedPhrase));
        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptAfterSecondOpinion
        );
        assert_eq!(routed.selected.text(), "yes yes yes yes");
    }

    #[test]
    fn low_confidence_only_yields_to_a_second_engine_that_reports_its_own_confidence() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "meet at the sluggish tail")
            .with_confidence(0.2);

        let silent = FakeProvider::new(TranscriptionEngine::Parakeet, "meet at the Slugtale");
        let routed = router_with(primary.clone(), silent, SecondOpinionMode::Automatic)
            .route(&speech_of_seconds(3.0))
            .unwrap();
        assert_eq!(routed.escalation, Some(EscalationReason::LowConfidence));
        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptAfterSecondOpinion
        );

        let confident = FakeProvider::new(TranscriptionEngine::Parakeet, "meet at the Slugtale")
            .with_confidence(0.93);
        let routed = router_with(primary, confident, SecondOpinionMode::Automatic)
            .route(&speech_of_seconds(3.0))
            .unwrap();
        assert_eq!(routed.selection, SelectionReason::SecondOpinionSelected);
        assert_eq!(routed.selected.text(), "meet at the Slugtale");
    }

    #[test]
    fn an_unavailable_second_engine_keeps_the_first_transcript() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::AppleSpeech, "never reached")
            .unavailable(EngineUnavailable::RuntimeNotBuilt);
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(routed.escalation, Some(EscalationReason::EmptyTranscript));
        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );
        assert_eq!(routed.selected.text(), "");
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn a_failing_second_engine_cannot_turn_a_usable_dictation_into_an_error() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "").failing();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );
        assert_eq!(
            routed.second_opinion_engine,
            Some(TranscriptionEngine::Parakeet)
        );
    }

    #[test]
    fn a_slow_second_engine_gives_up_at_the_budget_rather_than_holding_the_dictation() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "too late to matter")
            .slow(Duration::from_millis(400));
        let router = router_with(primary, second, SecondOpinionMode::Automatic).with_policy(EscalationPolicy::default().with_budget(Duration::from_millis(40)));

        let started = Instant::now();
        let routed = router.route(&speech_of_seconds(3.0)).unwrap();
        let waited = started.elapsed();

        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );
        assert!(
            waited < Duration::from_millis(300),
            "router waited {waited:?}, which is past its budget"
        );
    }

    #[test]
    fn a_timed_out_escalation_blocks_a_later_segments_router_too() {
        let coordinator = SecondOpinionCoordinator::default();
        let slow = FakeProvider::new(TranscriptionEngine::Parakeet, "too late")
            .slow(Duration::from_millis(600));
        let slow_calls = slow.calls.clone();
        let first_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(slow),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator.clone())
        .with_policy(EscalationPolicy::default().with_budget(Duration::from_millis(50)));

        let routed_first = first_segment.route(&speech_of_seconds(3.0)).unwrap();
        assert_eq!(
            routed_first.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );

        let next_second = FakeProvider::new(TranscriptionEngine::AppleSpeech, "never starts");
        let next_calls = next_second.calls.clone();
        let next_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(next_second),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator);

        let routed_next = next_segment.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(
            routed_next.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );
        assert_eq!(next_calls.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(slow_calls.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn routers_given_no_shared_coordinator_do_not_block_each_other() {
        let slow = FakeProvider::new(TranscriptionEngine::Parakeet, "too late")
            .slow(Duration::from_millis(600));
        let first_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(slow),
            SecondOpinionMode::Automatic,
        )
        .with_policy(EscalationPolicy::default().with_budget(Duration::from_millis(50)));
        assert_eq!(
            first_segment
                .route(&speech_of_seconds(3.0))
                .unwrap()
                .selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );

        let next_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(FakeProvider::new(TranscriptionEngine::AppleSpeech, "rescued")),
            SecondOpinionMode::Automatic,
        );

        assert_eq!(
            next_segment
                .route(&speech_of_seconds(3.0))
                .unwrap()
                .selection,
            SelectionReason::SecondOpinionSelected
        );
    }

    #[test]
    fn the_shared_gate_opens_again_once_the_real_worker_ends() {
        let coordinator = SecondOpinionCoordinator::default();
        let slow = FakeProvider::new(TranscriptionEngine::Parakeet, "slow answer")
            .slow(Duration::from_millis(200));
        let second_calls = slow.calls.clone();

        let first_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(slow),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator.clone())
        .with_policy(EscalationPolicy::default().with_budget(Duration::from_millis(30)));
        assert_eq!(
            first_segment
                .route(&speech_of_seconds(3.0))
                .unwrap()
                .selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );

        std::thread::sleep(Duration::from_millis(400));

        let later_segment = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(FakeProvider::new(TranscriptionEngine::AppleSpeech, "rescued")),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator);

        let routed = later_segment.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(routed.selection, SelectionReason::SecondOpinionSelected);
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn a_panicking_second_engine_releases_the_gate_and_keeps_the_primary() {
        let coordinator = SecondOpinionCoordinator::default();
        let router = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "kept")),
            Arc::new(PanickingProvider {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator.clone());

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();
        assert_eq!(
            routed.selection,
            SelectionReason::PrimaryKeptSecondOpinionUnavailable
        );
        assert_eq!(routed.selected.text(), "kept");

        let after = SecondOpinionRouter::new(
            Arc::new(FakeProvider::new(TranscriptionEngine::Whisper, "")),
            Arc::new(FakeProvider::new(TranscriptionEngine::AppleSpeech, "rescued")),
            SecondOpinionMode::Automatic,
        )
        .with_coordinator(coordinator);
        assert_eq!(
            after.route(&speech_of_seconds(3.0)).unwrap().selection,
            SelectionReason::SecondOpinionSelected
        );
    }

    struct PanickingProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl EngineTranscriber for PanickingProvider {
        fn engine(&self) -> TranscriptionEngine {
            TranscriptionEngine::Parakeet
        }

        fn metadata(&self) -> EngineMetadata {
            FakeProvider::new(TranscriptionEngine::Parakeet, "").metadata()
        }

        fn availability(&self) -> EngineAvailability {
            EngineAvailability::Available
        }

        fn transcribe(&self, _audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            panic!("fake engine panic");
        }
    }

    impl EngineAssetLifecycle for PanickingProvider {
        fn assets(&self) -> EngineAssets {
            EngineAssets {
                installed_bytes: None,
                present: Some(true),
            }
        }
    }

    #[test]
    fn a_primary_failure_is_still_a_failure() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "").failing();
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "unreachable");
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let error = router.route(&speech_of_seconds(3.0)).unwrap_err();

        assert_eq!(error, AsrError::Runtime("fake engine failure".to_string()));
    }

    #[test]
    fn routing_diagnostics_carry_reason_codes_and_no_speech() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "the rescued sentence");
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();
        let diagnostics = routed.diagnostics();
        let json = serde_json::to_string(&diagnostics).unwrap();

        assert_eq!(
            diagnostics.escalation,
            Some(EscalationReason::EmptyTranscript)
        );
        assert_eq!(
            diagnostics.selection,
            SelectionReason::SecondOpinionSelected
        );
        assert!(
            !json.contains("rescued"),
            "diagnostics leaked transcript text: {json}"
        );
        assert!(json.contains("empty-transcript"), "got: {json}");
        assert!(json.contains("second-opinion-selected"), "got: {json}");
    }

    #[test]
    fn repeated_phrase_detection_finds_loops_but_not_ordinary_repetition() {
        for looping in [
            "the the the",
            "I don't know. I don't know. I don't know.",
            "thank you thank you thank you thank you",
            "go on go on go on and on",
        ] {
            assert!(
                has_repeated_phrase(looping, 3),
                "should have caught the loop in {looping:?}"
            );
        }

        for ordinary in [
            "Book the meeting for Tuesday and tell the team",
            "very very good",
            "that that clause is fine in English",
            "",
        ] {
            assert!(
                !has_repeated_phrase(ordinary, 3),
                "should not have flagged {ordinary:?}"
            );
        }
    }

    #[test]
    fn far_too_little_text_for_a_long_recording_escalates() {
        let policy = EscalationPolicy::default();
        let long_recording = Duration::from_secs(20);

        assert_eq!(
            policy.escalation_for(&plain("ok"), long_recording),
            Some(EscalationReason::ImplausiblyShortForDuration)
        );
        assert_eq!(
            policy.escalation_for(
                &plain("please book the meeting room for the team on Tuesday morning"),
                long_recording
            ),
            None
        );
    }

    #[test]
    fn the_words_per_second_floor_is_the_number_the_router_uses() {
        let ten_seconds = Duration::from_secs(10);
        let four_words = plain("book the flight please");
        let rate = |policy: &EscalationPolicy| policy.escalation_for(&four_words, ten_seconds);

        assert_eq!(rate(&EscalationPolicy::default()), None);

        let stricter = EscalationPolicy::default().with_minimum_words_per_second(0.5);
        assert_eq!(
            rate(&stricter),
            Some(EscalationReason::ImplausiblyShortForDuration)
        );

        let looser = EscalationPolicy::default().with_minimum_words_per_second(0.3);
        assert_eq!(rate(&looser), None);
    }

    fn plain(text: &str) -> EngineTranscription {
        EngineTranscription::plain(
            TranscriptionEngine::Whisper,
            FinalTranscription::plain(text),
            Duration::from_millis(200),
        )
    }

    fn speech_of_seconds(seconds: f32) -> CapturedAudio {
        CapturedAudio::mono_16khz(vec![0.05; (16_000.0 * seconds) as usize])
    }

    fn router_with(
        primary: FakeProvider,
        second: FakeProvider,
        mode: SecondOpinionMode,
    ) -> SecondOpinionRouter {
        SecondOpinionRouter::new(Arc::new(primary), Arc::new(second), mode)
    }

    #[derive(Clone)]
    struct FakeProvider {
        engine: TranscriptionEngine,
        text: String,
        confidence: Option<f32>,
        availability: EngineAvailability,
        fails: bool,
        delay: Option<Duration>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeProvider {
        fn new(engine: TranscriptionEngine, text: &str) -> Self {
            Self {
                engine,
                text: text.to_string(),
                confidence: None,
                availability: EngineAvailability::Available,
                fails: false,
                delay: None,
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn with_confidence(mut self, confidence: f32) -> Self {
            self.confidence = Some(confidence);
            self
        }

        fn unavailable(mut self, reason: EngineUnavailable) -> Self {
            self.availability = EngineAvailability::Unavailable(reason);
            self
        }

        fn failing(mut self) -> Self {
            self.fails = true;
            self
        }

        fn slow(mut self, delay: Duration) -> Self {
            self.delay = Some(delay);
            self
        }
    }

    impl EngineTranscriber for FakeProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.engine
        }
        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.engine,
                model_id: "fake",
                capability: "test",
                revision: "fake",
                approximate_bytes: None,
                source_url: None,
                license: "fake",
                license_url: "fake",
                attribution: None,
                modifications: None,
                system_managed: false,
                supported_platforms: "test",
            }
        }

        fn availability(&self) -> EngineAvailability {
            self.availability.clone()
        }

        fn transcribe(&self, _audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            if self.fails {
                return Err(AsrError::Runtime("fake engine failure".to_string()));
            }
            Ok(EngineTranscription {
                engine: self.engine,
                transcription: FinalTranscription::plain(self.text.clone()),
                alternatives: Vec::new(),
                confidence: EngineConfidence {
                    mean: self.confidence,
                    minimum: self.confidence,
                },
                latency: Duration::from_millis(200),
            })
        }
    }

    impl EngineAssetLifecycle for FakeProvider {
        fn assets(&self) -> EngineAssets {
            EngineAssets {
                installed_bytes: None,
                present: Some(true),
            }
        }
    }
}
