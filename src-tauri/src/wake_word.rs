//! Voice Activation spike (slugtale-e95): deciding whether a transcript sounds
//! like the wake phrase "Hi Slugtale".
//!
//! The spike reuses the managed Whisper model on rolling microphone windows and
//! matches the resulting text against known mishearings of the phrase, so this
//! module is pure logic with no audio or model dependency and compiles (and
//! tests) on every platform. The always-on listener that feeds it lives behind
//! the `voice-activation` cargo feature in the Tauri tier.
//!
//! Everything here scores and logs only. No audio samples ever persist: a
//! window is dropped the moment it has been scored (ADR-0001, ADR-0019).

/// The phrase variants the matcher accepts as the wake phrase, pre-normalized
/// the same way incoming transcripts are normalized. Whisper mishears
/// "Slugtale" often enough ("slug tale", "slug tail") that an exact match alone
/// would reject most real attempts.
const WAKE_PHRASE_VARIANTS: [&str; 4] = [
    "hi slugtale",
    "hey slugtale",
    "hi slug tale",
    "hey slug tale",
];

/// Greeting tokens that raise the score when they sit before the app name.
const GREETING_TOKENS: [&str; 6] = ["hi", "hey", "hay", "high", "eyes", "i"];

/// Lowercase, strip punctuation, and collapse whitespace so "Hey, SlugTale!"
/// and "hey slugtale" land on the same string.
pub fn normalize_for_wake_match(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut last_was_space = true;
    for character in text.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            normalized.push(' ');
            last_was_space = true;
        }
    }
    normalized.trim_end().to_string()
}

fn levenshtein_at_most_one(a: &str, b: &str) -> bool {
    // Cheap subset of full edit distance: equal strings, single insert,
    // single delete, or single substitution. Enough for one-character
    // mishearings without dragging in a distance matrix.
    if a == b {
        return true;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (longer, shorter) = if a.len() >= b.len() { (&a, &b) } else { (&b, &a) };
    for position in 0..shorter.len() {
        if longer[position] != shorter[position] {
            return longer[position + 1..] == shorter[position..]
                || longer[position + 1..] == shorter[position + 1..];
        }
    }
    // The longer string is exactly the shorter plus one trailing character.
    true
}

fn token_fuzzy_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Short tokens ("i" vs "eye") are too noisy to fuzzy-match; require some
    // length before forgiving a single-character difference.
    if a.len().min(b.len()) < 4 {
        return false;
    }
    levenshtein_at_most_one(a, b)
}

fn tokens_fuzzy_eq_sequence(tokens: &[&str], expected: &[&str]) -> bool {
    tokens.len() == expected.len()
        && tokens
            .iter()
            .zip(expected)
            .all(|(token, want)| token_fuzzy_eq(token, want))
}

/// Whether one transcript token sounds like the app name.
///
/// Whisper's mishearings of "Slugtale" are rarely one edit away ("slugtail"
/// is two), but almost always keep the "slug" onset and roughly the same
/// length. Anything else keeps the strict rules above.
fn is_wake_name_token(token: &str) -> bool {
    token == "slugtale" || (token.len() >= 7 && token.starts_with("slug"))
}

/// Score how much a transcript sounds like the wake phrase, from 0.0 to 1.0.
///
/// A full phrase match (greeting included) scores highest; the app name alone
/// scores below the default trigger threshold, because users say "Slugtale"
/// in ordinary sentences far more often than they greet it.
pub fn wake_phrase_score(transcript: &str) -> f32 {
    let normalized = normalize_for_wake_match(transcript);
    if normalized.is_empty() {
        return 0.0;
    }

    for variant in WAKE_PHRASE_VARIANTS {
        if normalized == variant {
            return 1.0;
        }
    }

    let tokens: Vec<&str> = normalized.split(' ').collect();

    // Whole-transcript variants with one fuzzy difference, e.g.
    // "hi slugtail" or "hey slug tales".
    for variant in WAKE_PHRASE_VARIANTS {
        if tokens_fuzzy_eq_sequence(&tokens, variant.split(' ').collect::<Vec<_>>().as_slice()) {
            return 0.9;
        }
    }

    // The app name anywhere in the sentence: one token that sounds like
    // "Slugtale", or the two-token split Whisper sometimes produces
    // ("slug tale"/"slug tail").
    for index in 0..tokens.len() {
        let name_matched = is_wake_name_token(tokens[index])
            || (index + 1 < tokens.len()
                && tokens[index] == "slug"
                && (token_fuzzy_eq(tokens[index + 1], "tale")
                    || token_fuzzy_eq(tokens[index + 1], "tail")));
        if !name_matched {
            continue;
        }

        let greeted = index > 0 && GREETING_TOKENS.contains(&tokens[index - 1]);
        // "…the Slugtale settings…" must not trigger: a bare name mention is
        // well under the default threshold, a greeting pushes it over.
        return if greeted { 0.8 } else { 0.4 };
    }

    0.0
}

/// What one evaluated transcript decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeDetection {
    pub score: f32,
}

/// Tunables for [`WakeWordDetector`]. Defaults are deliberately strict: the
/// spike's job is to measure real false-accept rates, and a detector that
/// fires too eagerly would poison the dogfooding data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeWordConfig {
    /// Minimum [`wake_phrase_score`] that starts dictation.
    pub trigger_threshold: f32,
    /// Minimum milliseconds between triggers, so the tail of the wake phrase
    /// arriving in the next window cannot immediately fire again.
    pub cooldown_ms: u64,
}

impl Default for WakeWordConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: 0.8,
            cooldown_ms: 4_000,
        }
    }
}

/// The detection state machine: transcripts in, at most one trigger per
/// cooldown period out.
#[derive(Debug)]
pub struct WakeWordDetector {
    config: WakeWordConfig,
    last_trigger_ms: u64,
    has_triggered: bool,
}

impl WakeWordDetector {
    pub fn new(config: WakeWordConfig) -> Self {
        Self {
            config,
            last_trigger_ms: 0,
            has_triggered: false,
        }
    }

    /// Evaluate one transcript stamped with its wall-clock milliseconds.
    /// Returns a detection when the score clears the threshold and the
    /// cooldown has elapsed since the previous trigger.
    pub fn on_transcript(&mut self, transcript: &str, now_ms: u64) -> Option<WakeDetection> {
        let score = wake_phrase_score(transcript);
        if score < self.config.trigger_threshold {
            return None;
        }
        if self.has_triggered
            && now_ms.saturating_sub(self.last_trigger_ms) < self.config.cooldown_ms
        {
            return None;
        }
        self.last_trigger_ms = now_ms;
        self.has_triggered = true;
        Some(WakeDetection { score })
    }
}

/// How much recent audio the listener keeps for evaluation.
const WINDOW_SECONDS: usize = 4;
const SAMPLES_PER_SECOND: usize = 16_000;

/// Rolling buffer of mono 16 kHz samples feeding the listener's transcription
/// windows. Pure sample bookkeeping, so it is unit-testable without hardware.
#[derive(Default)]
pub struct SpeechWindowBuffer {
    samples: Vec<f32>,
    evaluated_up_to: usize,
}

impl SpeechWindowBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[f32]) {
        let capacity = WINDOW_SECONDS * SAMPLES_PER_SECOND;
        self.samples.extend_from_slice(chunk);
        let overflow = self.samples.len().saturating_sub(capacity);
        self.samples.drain(..overflow);
        self.evaluated_up_to = self.evaluated_up_to.saturating_sub(overflow);
    }

    /// Root mean square of everything buffered — the cheap energy gate that
    /// keeps Whisper off while the room is silent.
    pub fn rms(&self) -> f32 {
        crate::audio_capture::audio_level_from_samples(&self.samples)
    }

    /// Whether at least `min_new_samples` of un-evaluated audio have arrived
    /// since the last transcription, so the listener does not re-score the
    /// same audio forever.
    pub fn ready_for_evaluation(&self, min_new_samples: usize) -> bool {
        self.samples.len() - self.evaluated_up_to >= min_new_samples
    }

    /// Snapshot the current window and mark it evaluated. The caller drops the
    /// snapshot once scored; nothing here outlives the call.
    pub fn take_for_evaluation(&mut self) -> Vec<f32> {
        self.evaluated_up_to = self.samples.len();
        self.samples.clone()
    }

    /// Drop everything after a trigger, so the tail of the wake phrase cannot
    /// leak into the first dictation segment's audio or into later scoring.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.evaluated_up_to = 0;
    }

    /// Keep only the most recent `samples_to_keep` samples and mark them
    /// unevaluated. The listener calls this after each transcription so a wake
    /// phrase split across two windows is still scored whole, while audio that
    /// has already been transcribed is never scored twice.
    pub fn retain_recent(&mut self, samples_to_keep: usize) {
        let drop_count = self.samples.len().saturating_sub(samples_to_keep);
        if drop_count > 0 {
            self.samples.drain(..drop_count);
        }
        self.evaluated_up_to = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_lowercases_and_strips_punctuation() {
        assert_eq!(
            normalize_for_wake_match("Hey, SlugTale!"),
            "hey slugtale".to_string()
        );
        assert_eq!(normalize_for_wake_match("..."), String::new());
    }

    #[test]
    fn exact_variants_score_full_marks() {
        assert_eq!(wake_phrase_score("Hi Slugtale."), 1.0);
        assert_eq!(wake_phrase_score("hey slugtale"), 1.0);
        assert_eq!(wake_phrase_score("HI SLUG TALE"), 1.0);
    }

    #[test]
    fn one_character_mishearings_of_the_whole_phrase_still_score_high() {
        let score = wake_phrase_score("hi slugtail");
        assert!(score >= 0.8, "mishearing should stay high, got {score}");
    }

    #[test]
    fn a_greeted_name_mention_clears_the_default_threshold() {
        assert_eq!(wake_phrase_score("so hi slugtale then"), 0.8);
    }

    #[test]
    fn a_bare_name_mention_stays_below_the_default_threshold() {
        // Ordinary sentences contain the app name constantly; they must not
        // start dictation.
        assert_eq!(wake_phrase_score("the slugtale settings pane"), 0.4);
        assert!(wake_phrase_score("the slugtale settings pane") < 0.8);
    }

    #[test]
    fn unrelated_speech_scores_zero() {
        assert_eq!(wake_phrase_score("what time is the meeting"), 0.0);
        assert_eq!(wake_phrase_score(""), 0.0);
    }

    #[test]
    fn short_tokens_are_not_fuzzy_matched() {
        // "I" misheard as "a" must not turn "a slugtale" into a greeting.
        assert_eq!(wake_phrase_score("a slugtale"), 0.4);
    }

    #[test]
    fn detector_triggers_once_then_respects_the_cooldown() {
        let mut detector =
            WakeWordDetector::new(WakeWordConfig::default());

        let first = detector.on_transcript("hi slugtale", 1_000);
        assert!(first.is_some());
        assert_eq!(first.unwrap().score, 1.0);

        // Inside the cooldown: suppressed even at full score.
        assert!(detector.on_transcript("hi slugtale", 3_000).is_none());

        // After the cooldown: triggers again.
        assert!(detector.on_transcript("hey slugtale", 6_000).is_some());
    }

    #[test]
    fn detector_ignores_below_threshold_transcripts_entirely() {
        let mut detector = WakeWordDetector::new(WakeWordConfig::default());

        assert!(detector.on_transcript("the slugtale docs", 1_000).is_none());
        // And a low-scoring transcript never opens a cooldown window either.
        assert!(detector.on_transcript("hi slugtale", 1_100).is_some());
    }

    #[test]
    fn window_buffer_keeps_only_recent_samples_and_tracks_evaluation() {
        let mut window = SpeechWindowBuffer::new();
        window.push(&vec![0.5f32; SAMPLES_PER_SECOND * 3]);

        assert!(window.ready_for_evaluation(SAMPLES_PER_SECOND * 2));
        let snapshot = window.take_for_evaluation();
        assert_eq!(snapshot.len(), SAMPLES_PER_SECOND * 3);
        assert!(!window.ready_for_evaluation(SAMPLES_PER_SECOND));

        // Pushing five seconds keeps only the newest four.
        window.push(&vec![0.25f32; SAMPLES_PER_SECOND * 2]);
        assert_eq!(window.samples.len(), WINDOW_SECONDS * SAMPLES_PER_SECOND);
        assert!(window.ready_for_evaluation(SAMPLES_PER_SECOND));
    }

    #[test]
    fn clearing_the_window_drops_pending_audio_after_a_trigger() {
        let mut window = SpeechWindowBuffer::new();
        window.push(&[0.4, -0.4]);
        window.clear();
        assert!(!window.ready_for_evaluation(1));
        assert_eq!(window.rms(), 0.0);
    }

    #[test]
    fn window_rms_reports_energy_for_the_energy_gate() {
        let mut window = SpeechWindowBuffer::new();
        assert_eq!(window.rms(), 0.0);
        window.push(&[1.0, -1.0, 1.0, -1.0]);
        assert!((window.rms() - 1.0).abs() < 1e-6);
    }
}
