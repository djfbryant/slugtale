
use crate::audio_capture::is_voice_level;

/// Watches the dictation's voice level and decides when a Segment Pause has
/// elapsed.
///
/// The detector only ever fires after it has actually heard speech, and it
/// requires new speech before it will fire again. That single rule is what keeps
/// a dictation that opens with silence, and a user who walks away mid-dictation,
/// from producing a stream of empty insertions.
pub struct SegmentPauseDetector {
    pause: std::time::Duration,
    last_voice: Option<std::time::Instant>,
}

impl SegmentPauseDetector {
    pub fn with_pause(pause: std::time::Duration) -> Self {
        Self {
            pause,
            last_voice: None,
        }
    }

    pub fn set_pause(&mut self, pause: std::time::Duration) {
        self.pause = pause;
    }

    #[cfg(test)]
    pub(crate) fn pause(&self) -> std::time::Duration {
        self.pause
    }

    pub fn rearm(&mut self) {
        self.last_voice = None;
    }

    /// Feed one voice level sampled at `at`, and report whether a Segment Pause
    /// has just completed. `true` means the audio captured so far should become
    /// a Dictation Segment now.
    ///
    /// Firing re-arms the detector: it will not fire again until it has heard
    /// speech again, so a long silence produces exactly one flush.
    pub fn on_level(&mut self, level: f32, at: std::time::Instant) -> bool {
        if is_voice_level(level) {
            self.last_voice = Some(at);
            return false;
        }

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

    const TEST_PAUSE: Duration = Duration::from_millis(500);

    fn speaking() -> f32 {
        crate::audio_capture::VOICE_LEVEL + 0.2
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
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();

        for tick in 0..40 {
            let at = start + Duration::from_millis(tick * 100);
            assert!(!detector.on_level(quiet(), at), "fired at tick {tick}");
        }
    }

    #[test]
    fn a_long_silence_ends_exactly_one_segment() {
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
        let threshold = crate::audio_capture::VOICE_LEVEL;
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);
        let start = std::time::Instant::now();
        detector.on_level(speaking(), start);

        assert!(!detector.on_level(threshold, start + Duration::from_millis(100)));
        assert!(detector.on_level(threshold, start + Duration::from_millis(700)));
    }

    #[test]
    fn rearming_leaves_the_detector_exactly_as_a_fresh_one() {
        let start = std::time::Instant::now();
        let mut used = SegmentPauseDetector::with_pause(TEST_PAUSE);
        used.on_level(speaking(), start);
        used.rearm();

        let mut fresh = SegmentPauseDetector::with_pause(TEST_PAUSE);

        let mut level = quiet();
        for tick in 0..30 {
            if tick == 7 {
                level = speaking();
            }
            if tick == 12 {
                level = quiet();
            }
            let at = start + Duration::from_millis(tick * 100);
            assert_eq!(
                used.on_level(level, at),
                fresh.on_level(level, at),
                "tick {tick} answered differently after rearm"
            );
        }
    }

    #[test]
    fn a_new_pause_measures_from_the_next_rearm_onward() {
        let start = std::time::Instant::now();
        let mut detector = SegmentPauseDetector::with_pause(TEST_PAUSE);

        detector.set_pause(TEST_PAUSE * 2);
        detector.rearm();
        detector.on_level(speaking(), start);

        assert!(!detector.on_level(quiet(), start + TEST_PAUSE));
        assert!(detector.on_level(quiet(), start + TEST_PAUSE * 2));
    }
}
