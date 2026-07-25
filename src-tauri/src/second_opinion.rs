//! Second Opinion routing (slugtale-vjs.3): run one Transcription Engine
//! normally, and ask a second local engine only when the first result looks
//! uncertain or anomalous.
//!
//! The shape of this is deliberate. Asking two engines to transcribe every
//! dictation would double the wait on a machine that already answers in a few
//! hundred milliseconds, and it still gives no answer when the two disagree.
//! Asking a second listener only when the first one says "I think that was
//! *slug tail*" costs nothing on the normal path and helps exactly where help
//! is needed (docs/research/2026-07-24-small-local-asr-and-model-collaboration.md).
//!
//! Three properties this module is built to guarantee:
//!
//! 1. **Exactly one transcript is inserted.** Slugtale selects between complete
//!    transcripts; it never merges words from two engines. Word-level fusion is
//!    a later experiment that first needs evidence of an oracle gap.
//! 2. **The rules are fixed and inspectable.** Every escalation and every
//!    selection produces a non-content reason code, so a user or a maintainer
//!    can ask "why did it do that?" and get an answer that contains no speech.
//! 3. **The first usable transcript always survives.** A second opinion that
//!    times out, errors, or is unavailable can only ever fail to improve the
//!    result — it can never lose the dictation.

use crate::{
    captured_audio_duration, AsrError, CapturedAudio, EngineTranscription, TranscriptionEngine,
    TranscriptionProvider,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Whether Slugtale may ask a second local engine for another opinion. Stored
/// in the Settings File; `Off` is the default and reproduces today's behaviour
/// exactly — one engine, no router overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecondOpinionMode {
    /// Only the primary engine ever runs.
    Off,
    /// A second engine runs when, and only when, the primary result trips one
    /// of the escalation rules below.
    Automatic,
}

impl Default for SecondOpinionMode {
    /// Off until slugtale-9dv has measured that escalation actually helps, and
    /// which thresholds to use. Shipping a second engine on by default would
    /// spend the user's battery on an unproven benefit.
    fn default() -> Self {
        Self::Off
    }
}

/// Why the primary engine's result was not trusted on its own.
///
/// Every variant names a property of the *result shape*, never its content, so
/// these are safe to write to the Local Diagnostic Log and to show the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationReason {
    /// The engine returned nothing for a recording long enough to contain
    /// speech. The most clearly recoverable failure there is.
    EmptyTranscript,
    /// The engine's own confidence fell below its threshold. Compared only
    /// against that engine's threshold — never against another engine's score,
    /// which is on a different scale until calibrated.
    LowConfidence,
    /// The same short phrase repeats several times in a row. Whisper's
    /// characteristic failure: it loops rather than admitting it heard nothing.
    RepeatedPhrase,
    /// Far too few words came back for how long the user spoke.
    ImplausiblyShortForDuration,
}

/// Why the router inserted the transcript it did. Non-content, like
/// [`EscalationReason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionReason {
    /// Nothing looked wrong, so no second engine ran. The normal path.
    PrimaryAccepted,
    /// A second opinion ran and the primary still read better under the rules.
    PrimaryKeptAfterSecondOpinion,
    /// A second opinion ran and read better, so it replaced the primary.
    SecondOpinionSelected,
    /// A second opinion was wanted but could not run — unavailable engine,
    /// error, timeout, or one already in flight. The primary's flawed
    /// transcript is still the best available, so it is inserted rather than
    /// losing the dictation.
    PrimaryKeptSecondOpinionUnavailable,
}

/// The router's complete, non-content account of one dictation, plus the single
/// transcript to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedTranscription {
    /// The one transcript to insert. Never a merge of two engines.
    pub selected: EngineTranscription,
    /// Which rule fired, if any. `None` means the primary was trusted outright.
    pub escalation: Option<EscalationReason>,
    pub selection: SelectionReason,
    /// Which engine was asked for a second opinion, when one was asked.
    pub second_opinion_engine: Option<TranscriptionEngine>,
    /// Wall-clock time for the whole routed dictation, including any escalation.
    pub total_latency: Duration,
}

impl RoutedTranscription {
    /// The non-content summary safe to record in the Local Diagnostic Log.
    /// Deliberately returns owned copies of only the codes and the timings, so
    /// a caller cannot reach the transcript through it by accident.
    pub fn diagnostics(&self) -> RoutingDiagnostics {
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
    /// The engine whose transcript was actually inserted — which is the second
    /// opinion whenever it won, not the primary.
    pub selected_engine: TranscriptionEngine,
    pub escalation: Option<EscalationReason>,
    pub selection: SelectionReason,
    pub second_opinion_engine: Option<TranscriptionEngine>,
    pub total_latency_ms: u64,
}

/// The fixed rules that decide whether a result deserves a second opinion.
///
/// These are thresholds, not a learned model, so a maintainer can read them and
/// predict the behaviour. The defaults are deliberately conservative: they are
/// placeholders until slugtale-9dv measures real escalation rates, and an
/// over-eager router costs every user latency to rescue a few dictations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscalationPolicy {
    /// Escalate below this confidence. Only consulted for engines that actually
    /// report confidence; a silent engine is not an uncertain one.
    pub minimum_confidence: f32,
    /// Escalate below this many words per second of recording. Ordinary speech
    /// runs 2–3 words per second, so this only catches near-total loss rather
    /// than someone speaking slowly.
    pub minimum_words_per_second: f32,
    /// Recordings shorter than this are not judged on length or emptiness. A
    /// half-second of silence legitimately transcribes to nothing, and treating
    /// that as a failure would escalate every accidental hotkey press.
    pub minimum_judged_duration: Duration,
    /// How many consecutive repeats of the same short phrase count as a loop.
    pub repeated_phrase_run: usize,
    /// How long a second opinion may take before the router gives up and keeps
    /// the first result. Bounded so a slow or wedged engine can never hold a
    /// dictation hostage.
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
    /// The first rule this result trips, or `None` when it looks healthy.
    ///
    /// Rules are checked in order of how confident we are that they indicate a
    /// real failure, so the reported reason is the strongest explanation rather
    /// than whichever happened to be checked first.
    pub fn escalation_for(
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
    /// The primary is deliberately not read here. Every rule below asks only
    /// "is the second opinion good enough to displace what we already have?",
    /// judged against an absolute standard. Keeping the primary in the
    /// signature documents that it is the incumbent and the default answer.
    pub fn select(
        &self,
        _primary: &EngineTranscription,
        second: &EngineTranscription,
        escalation: EscalationReason,
        audio_duration: Duration,
    ) -> SelectionReason {
        let second_is_healthy = self.escalation_for(second, audio_duration).is_none();

        let second_wins = match escalation {
            // The primary gave us nothing, so anything intelligible is better.
            EscalationReason::EmptyTranscript => !second.text().trim().is_empty(),
            // Take the second opinion only if it did not loop as well.
            EscalationReason::RepeatedPhrase => {
                !has_repeated_phrase(second.text().trim(), self.repeated_phrase_run)
                    && !second.text().trim().is_empty()
            }
            // Prefer the transcript with a plausible amount of text for the
            // recording — but only when the second one is plausible outright,
            // never merely longer, so a hallucinating engine cannot win by
            // producing more words.
            EscalationReason::ImplausiblyShortForDuration => second_is_healthy,
            // Confidence is the one signal we cannot compare across engines, so
            // the second opinion must clear its *own* threshold and show no
            // other anomaly. An engine that reports no confidence at all cannot
            // clear it, and the primary is kept.
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

/// Runs the primary engine and, when its result trips a rule, one second
/// opinion. Owns the escalation budget and the guarantee that exactly one
/// transcript comes out.
pub struct SecondOpinionRouter {
    primary: Arc<dyn TranscriptionProvider>,
    second: Option<Arc<dyn TranscriptionProvider>>,
    mode: SecondOpinionMode,
    policy: EscalationPolicy,
    /// Set while a second opinion is running. A dictation that starts while an
    /// earlier escalation is still decoding skips its own escalation rather
    /// than putting two model runtimes on the CPU at once — on the 8 GB
    /// reference machine that would slow the dictation the user is waiting on.
    escalation_in_flight: Arc<std::sync::atomic::AtomicBool>,
    observer: Option<Arc<dyn Fn(RoutingDiagnostics) + Send + Sync>>,
}

impl SecondOpinionRouter {
    /// A router that only ever runs one engine. This is the shape the app uses
    /// when Second Opinion is Off, and it costs nothing beyond the primary call.
    pub fn single(primary: Arc<dyn TranscriptionProvider>) -> Self {
        Self {
            primary,
            second: None,
            mode: SecondOpinionMode::Off,
            policy: EscalationPolicy::default(),
            escalation_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            observer: None,
        }
    }

    pub fn new(
        primary: Arc<dyn TranscriptionProvider>,
        second: Arc<dyn TranscriptionProvider>,
        mode: SecondOpinionMode,
    ) -> Self {
        Self {
            primary,
            second: Some(second),
            mode,
            policy: EscalationPolicy::default(),
            escalation_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            observer: None,
        }
    }

    pub fn with_policy(mut self, policy: EscalationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Report every routing decision to a non-content observer — in the app,
    /// the Local Diagnostic Log.
    ///
    /// The observer receives [`RoutingDiagnostics`], which is a closed set of
    /// codes and a duration. That type, rather than the router's discipline, is
    /// what makes it impossible to log the user's speech from here.
    pub fn observing(
        mut self,
        observer: impl Fn(RoutingDiagnostics) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    /// Route one dictation, escalating only if the primary result trips a rule.
    /// Fails only when the *primary* fails: once there is a usable first
    /// transcript, nothing the second engine does can turn this into an error.
    ///
    /// Named `route` rather than `transcribe` so it reads distinctly from the
    /// [`crate::AsrRuntime`] implementation below, which discards the reason
    /// codes this returns.
    pub fn route(&self, audio: &CapturedAudio) -> Result<RoutedTranscription, AsrError> {
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

        let Some(second) = self.second_opinion_within_budget(second_provider, audio) else {
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

    /// Whether this result should be escalated at all. Returns `None` the moment
    /// Second Opinion is Off or no second engine is configured, so the normal
    /// path does not even inspect the transcript.
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

    /// The second engine, if it is configured and can actually run right now.
    /// Availability is checked before decoding rather than after, so an engine
    /// whose assets were removed costs nothing.
    fn available_second_opinion(&self) -> Option<Arc<dyn TranscriptionProvider>> {
        let second = self.second.as_ref()?;
        second.availability().is_available().then(|| second.clone())
    }

    /// Run the second engine, giving up at the budget.
    ///
    /// The work runs on its own thread so the router can stop waiting; a wedged
    /// engine keeps its thread until it returns, but it can no longer hold up
    /// the user's dictation, and `escalation_in_flight` stops a second one from
    /// piling on behind it. The recording is cloned because the thread outlives
    /// this call — that allocation is the price of a bounded wait, and it only
    /// happens on the escalation path, which is rare by design.
    fn second_opinion_within_budget(
        &self,
        provider: Arc<dyn TranscriptionProvider>,
        audio: &CapturedAudio,
    ) -> Option<EngineTranscription> {
        use std::sync::atomic::Ordering;

        if self
            .escalation_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }

        let in_flight = self.escalation_in_flight.clone();
        let audio = audio.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = provider.transcribe(&audio);
            in_flight.store(false, Ordering::Release);
            // A full channel means the router already gave up and moved on;
            // dropping the late result is exactly what should happen.
            let _ = sender.try_send(result);
        });

        receiver
            .recv_timeout(self.policy.second_opinion_budget)
            .ok()
            .and_then(|result| result.ok())
    }
}

/// Lets the router stand in wherever the Dictation Workflow expects a single
/// engine, so adding Second Opinion changed no part of the insertion path.
///
/// The reason codes are dropped here rather than lost: [`SecondOpinionRouter::observing`]
/// has already handed them to the Local Diagnostic Log by the time this
/// returns, and the workflow itself has no use for them.
impl crate::AsrRuntime for SecondOpinionRouter {
    fn transcribe(&self, audio: CapturedAudio) -> Result<crate::FinalTranscription, AsrError> {
        Ok(self.route(&audio)?.selected.transcription)
    }
}

/// Whether the same short phrase repeats `minimum_run` times back to back.
///
/// Checks phrases of one to four words because that is the shape engines
/// actually loop in — "the the the", or "I don't know. I don't know. I don't
/// know." Comparison ignores case and punctuation so a looped sentence is still
/// caught when the engine punctuates each repeat differently. The scan is
/// quadratic in the word count, which is irrelevant at a dictation's length.
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
        EngineAvailability, EngineConfidence, EngineMetadata, EngineUnavailable, FinalTranscription,
    };

    #[test]
    fn second_opinion_is_off_until_the_benchmark_says_otherwise() {
        // Shipping a second engine on by default would spend every user's
        // battery on a benefit slugtale-9dv has not yet measured.
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
        // Off must reproduce today's behaviour exactly: one engine, whatever it
        // says, even when the result is obviously broken.
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
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "Book the meeting for Tuesday");
        let second_calls = second.calls.clone();
        let router = router_with(primary, second, SecondOpinionMode::Automatic);

        let routed = router.route(&speech_of_seconds(3.0)).unwrap();

        assert_eq!(routed.escalation, Some(EscalationReason::EmptyTranscript));
        assert_eq!(routed.selection, SelectionReason::SecondOpinionSelected);
        assert_eq!(routed.selected.text(), "Book the meeting for Tuesday");
        assert_eq!(routed.selected.engine, TranscriptionEngine::Parakeet);
        // Exactly one second opinion, never a retry loop.
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn a_brief_recording_is_allowed_to_transcribe_to_nothing() {
        // An accidental hotkey tap is silence, not a failure. Escalating it
        // would run a second model every time the user brushed the key.
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
        // Both engines failed the same way. Swapping one loop for another is
        // churn, so the deterministic tie-break keeps the primary.
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
        // Confidence from two engines is not on the same scale until it has been
        // calibrated on the same recordings, so the second opinion must clear
        // its own threshold. An engine that reports nothing cannot clear it.
        let primary =
            FakeProvider::new(TranscriptionEngine::Whisper, "meet at the sluggish tail")
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

        let confident =
            FakeProvider::new(TranscriptionEngine::Parakeet, "meet at the Slugtale")
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
        // Availability is checked before decoding, so an unavailable engine
        // costs nothing at all.
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
        assert_eq!(routed.second_opinion_engine, Some(TranscriptionEngine::Parakeet));
    }

    #[test]
    fn a_slow_second_engine_gives_up_at_the_budget_rather_than_holding_the_dictation() {
        let primary = FakeProvider::new(TranscriptionEngine::Whisper, "");
        let second = FakeProvider::new(TranscriptionEngine::Parakeet, "too late to matter")
            .slow(Duration::from_millis(400));
        let router = router_with(primary, second, SecondOpinionMode::Automatic).with_policy(
            EscalationPolicy {
                second_opinion_budget: Duration::from_millis(40),
                ..EscalationPolicy::default()
            },
        );

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
    fn a_primary_failure_is_still_a_failure() {
        // The router rescues bad transcripts, not a dead primary engine. The
        // caller needs the real error so the Dictation Workflow can report it.
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

        assert_eq!(diagnostics.escalation, Some(EscalationReason::EmptyTranscript));
        assert_eq!(diagnostics.selection, SelectionReason::SecondOpinionSelected);
        // The whole point of the reason codes: a maintainer can read why the
        // router acted without the log ever holding what the user said.
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
        // Slow, deliberate speech is not a failure — the threshold sits well
        // below the 2–3 words per second of ordinary dictation.
        assert_eq!(
            policy.escalation_for(
                &plain("please book the meeting room for the team on Tuesday morning"),
                long_recording
            ),
            None
        );
    }

    fn plain(text: &str) -> EngineTranscription {
        EngineTranscription::plain(
            TranscriptionEngine::Whisper,
            FinalTranscription {
                text: text.to_string(),
            },
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

    impl TranscriptionProvider for FakeProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.engine
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.engine,
                model_id: "fake",
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
                transcription: FinalTranscription {
                    text: self.text.clone(),
                },
                alternatives: Vec::new(),
                confidence: EngineConfidence {
                    mean: self.confidence,
                    minimum: self.confidence,
                },
                latency: Duration::from_millis(200),
            })
        }
    }
}
