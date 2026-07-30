//! Dictation Segments and the Segment Pause that ends them (CONTEXT.md,
//! ADR-0015).
//!
//! A dictation used to be silent until the user stopped: capture ran, Whisper
//! ran on the whole buffer, and one Immediate Insertion happened. Anything
//! longer than a sentence or two meant talking into a void. This module owns the
//! one decision that changes that — when the user has been quiet long enough
//! that the speech so far is worth transcribing and inserting while the
//! microphone keeps running.
//!
//! It is deliberately pure. It sees the same perceptual voice level the
//! Dictation Bar renders and answers a single question, so the rule can be
//! tested at real timescales without a microphone, a clock, or a thread.

/// How long the user must stay below [`SEGMENT_VOICE_LEVEL`] before the speech
/// so far becomes its own Dictation Segment.
///
/// Fixed rather than configurable for now: five seconds is long enough that
/// ordinary between-sentence breathing does not trigger it, and short enough
/// that a paragraph lands while the user is still thinking about the next one.
/// A setting can follow once the behaviour has been lived with.
pub const SEGMENT_PAUSE: std::time::Duration = std::time::Duration::from_secs(5);

/// The perceptual voice level above which the user counts as speaking.
///
/// This is the Dictation Bar's own `VOICE_LEVEL` (src/dictation-bar.html), and
/// it has to stay that way: the bar visibly flexes its waveform on exactly the
/// input that keeps a Segment Pause from firing, so a user watching the bar can
/// see why a flush did or did not happen. Note this is a *perceptual* level from
/// `voice_level_from_rms`, not raw microphone RMS.
pub const SEGMENT_VOICE_LEVEL: f32 = 0.08;

/// Watches the dictation's voice level and decides when a Segment Pause has
/// elapsed.
///
/// The detector only ever fires after it has actually heard speech, and it
/// requires new speech before it will fire again. That single rule is what keeps
/// a dictation that opens with silence, and a user who walks away mid-dictation,
/// from producing a stream of empty insertions.
pub struct SegmentPauseDetector {
    pause: std::time::Duration,
    /// When the user was last heard speaking, or `None` when no speech has
    /// arrived since the detector was armed. The `None` case is load-bearing: it
    /// is simultaneously "this dictation has not started yet" and "the last
    /// pause has already been flushed", and both must stay silent.
    last_voice: Option<std::time::Instant>,
}

impl Default for SegmentPauseDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SegmentPauseDetector {
    pub fn new() -> Self {
        Self::with_pause(SEGMENT_PAUSE)
    }

    /// A detector with a non-default pause. Tests use this to exercise the rule
    /// without waiting five real seconds.
    pub fn with_pause(pause: std::time::Duration) -> Self {
        Self {
            pause,
            last_voice: None,
        }
    }

    /// Feed one voice level sampled at `at`, and report whether a Segment Pause
    /// has just completed. `true` means the audio captured so far should become
    /// a Dictation Segment now.
    ///
    /// Firing re-arms the detector: it will not fire again until it has heard
    /// speech again, so a long silence produces exactly one flush.
    pub fn on_level(&mut self, level: f32, at: std::time::Instant) -> bool {
        if level > SEGMENT_VOICE_LEVEL {
            self.last_voice = Some(at);
            return false;
        }

        // The pause is measured from the last word, not from the first quiet
        // sample, so "five seconds since you stopped talking" means exactly that
        // however often levels happen to arrive.
        let Some(last_voice) = self.last_voice else {
            return false;
        };
        if at.saturating_duration_since(last_voice) < self.pause {
            return false;
        }

        self.last_voice = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A short pause keeps the tests at test speed; the rule under test is the
    /// same one the five-second default drives.
    const TEST_PAUSE: Duration = Duration::from_millis(500);

    fn speaking() -> f32 {
        SEGMENT_VOICE_LEVEL + 0.2
    }

    fn quiet() -> f32 {
        0.0
    }

    #[test]
    fn a_pause_after_speech_ends_a_dictation_segment() {
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();

        assert!(!detector.on_level(speaking(), start));
        assert!(!detector.on_level(quiet(), start + Duration::from_millis(100)));
        assert!(!detector.on_level(quiet(), start + Duration::from_millis(400)));

        assert!(detector.on_level(quiet(), start + Duration::from_millis(500)));
    }

    #[test]
    fn silence_before_any_speech_never_ends_a_segment() {
        // A dictation that opens with the user still gathering their thoughts
        // must not insert an empty transcription five seconds in.
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();

        for tick in 0..40 {
            let at = start + Duration::from_millis(tick * 100);
            assert!(!detector.on_level(quiet(), at), "fired at tick {tick}");
        }
    }

    #[test]
    fn a_long_silence_ends_exactly_one_segment() {
        // Walking away mid-dictation must not produce a flush every five
        // seconds; the next one waits for the user to speak again.
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();
        detector.on_level(speaking(), start);

        let fires = (1..40)
            .filter(|tick| detector.on_level(quiet(), start + Duration::from_millis(tick * 100)))
            .count();

        assert_eq!(fires, 1);
    }

    #[test]
    fn speaking_again_arms_the_next_segment() {
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();

        detector.on_level(speaking(), start);
        assert!(detector.on_level(quiet(), start + Duration::from_millis(600)));

        detector.on_level(speaking(), start + Duration::from_millis(700));
        assert!(!detector.on_level(quiet(), start + Duration::from_millis(800)));
        assert!(detector.on_level(quiet(), start + Duration::from_millis(1_400)));
    }

    #[test]
    fn brief_gaps_between_sentences_do_not_end_a_segment() {
        // Ordinary breathing between sentences is shorter than the pause, and
        // each new word restarts the count.
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();

        for tick in 0..20 {
            let at = start + Duration::from_millis(tick * 100);
            let level = if tick % 4 == 0 { speaking() } else { quiet() };
            assert!(!detector.on_level(level, at), "fired at tick {tick}");
        }
    }

    #[test]
    fn the_voice_threshold_matches_the_dictation_bar() {
        // The bar treats *strictly above* 0.08 as voice. A level sitting exactly
        // on the threshold is room noise to both, so it must not hold a pause
        // open — otherwise a steady hum would silently disable flushing.
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();
        detector.on_level(speaking(), start);

        assert!(!detector.on_level(SEGMENT_VOICE_LEVEL, start + Duration::from_millis(100)));
        assert!(detector.on_level(SEGMENT_VOICE_LEVEL, start + Duration::from_millis(700)));
    }

    #[test]
    fn the_default_pause_is_five_seconds() {
        assert_eq!(SEGMENT_PAUSE, Duration::from_secs(5));

        let mut detector = SegmentPauseDetector::new();
        let start = std::time::Instant::now();
        detector.on_level(speaking(), start);

        assert!(!detector.on_level(quiet(), start + Duration::from_secs(4)));
        assert!(detector.on_level(quiet(), start + Duration::from_secs(5)));
    }
}
