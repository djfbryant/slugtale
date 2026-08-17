//! The Typing Baseline (CONTEXT.md, ADR-0025): the typing speed Time Saved is
//! measured against.
//!
//! It lives in the Settings File rather than the Usage File on purpose. Turning
//! Usage off deletes the counts, but a measurement the user sat through three
//! times is not something to throw away with them — and the Typing Challenges
//! work whether or not anything is being stored.
//!
//! Two ways to have one, and they are not equal. A typed estimate is a stand-in
//! that gets Time Saved off the ground; three completed Typing Challenges are a
//! measurement, and once measured the estimate can no longer override it.

use serde::{Deserialize, Serialize};

/// How many Typing Challenges make a measured baseline. Three, so one bad run —
/// a phone call, a misread line — is outvoted by the median rather than averaged
/// into the answer.
pub const TYPING_CHALLENGE_COUNT: usize = 3;

/// How long one Typing Challenge runs.
pub const TYPING_CHALLENGE_SECONDS: u32 = 30;

/// The lowest and highest typed estimate Slugtale will accept. Outside this
/// range the number is not a typing speed, and Time Saved would be nonsense.
pub const TYPED_ESTIMATE_RANGE: std::ops::RangeInclusive<u32> = 10..=150;

/// The English prose the Typing Challenges use, one passage per challenge.
///
/// These ship in the app. Nothing is downloaded and nothing is sent anywhere,
/// which is the same Local-Only Processing promise the dictation path makes.
/// Ordinary prose rather than a pangram or word list: the point is to measure
/// how the user types real sentences, which is what they dictate instead of.
pub const TYPING_CHALLENGE_PASSAGES: [&str; TYPING_CHALLENGE_COUNT] = [
    "The harbour was quiet that morning, and the boats leaned together at their \
     moorings as if sharing a long and complicated secret. A woman walked the \
     length of the pier with her coat buttoned to the collar, stopping once to \
     watch the water move under the boards. Further out, a bell rang twice and \
     then thought better of it. She had come here every winter since she was a \
     girl, and every winter the town seemed a little smaller and the water a \
     little wider, until the two of them met somewhere she could no longer point \
     to on a map or explain to anybody who had not seen it for themselves.",
    "Every good map is an argument about what matters. The cartographer decides \
     which rivers deserve a name, which roads are worth the ink, and which empty \
     stretches can be left empty. Read enough of them and you begin to see the \
     shape of the people who drew them. A survey made for tax collectors shows \
     you fields and fences; one made for sailors shows you rocks and depths and \
     almost nothing of the land at all. Neither is lying. They are simply \
     answering different questions, and a map that tried to answer all of them \
     at once would be no use to anyone.",
    "He kept the workshop exactly as his father had left it, down to the order of \
     the chisels on the wall. Visitors thought it was sentiment. It was simply \
     that he had learned to work in that room, and his hands still reached for \
     things where they had always been. Moving the plane to a tidier shelf would \
     have cost him a second every time he wanted it, and a second is a great deal \
     when you are trying to hold a line true. The dust smelled of oak and old \
     varnish, and the light came in low across the bench for about an hour each \
     afternoon before it gave up for the day.",
];

/// One finished Typing Challenge, scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypingChallengeResult {
    /// Which passage this run used, so a retry replaces the right slot and the
    /// three results stay matched to the three passages in order.
    pub passage_index: usize,
    /// Correct whitespace words per minute for this run.
    pub words_per_minute: u32,
}

/// The Typing Baseline as stored in the Settings File.
///
/// Both fields can be set at once, and that is not a contradiction: a user can
/// type an estimate, use Time Saved for a week, then sit the challenges. From
/// that moment the measurement wins and the estimate is inert history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypingBaseline {
    /// Completed Typing Challenges, at most one per passage. Fewer than three is
    /// partial progress: closing the window keeps what was finished.
    #[serde(default)]
    pub challenges: Vec<TypingChallengeResult>,
    /// The user's own guess at their typing speed, in words per minute.
    #[serde(default)]
    pub typed_estimate: Option<u32>,
}

impl TypingBaseline {
    /// The measured baseline, or `None` until all three challenges are done.
    ///
    /// The median rather than the mean: three runs is too few for an average to
    /// survive one bad one, and a bad run is always slow, never fast.
    pub fn measured_wpm(&self) -> Option<u32> {
        if self.challenges.len() < TYPING_CHALLENGE_COUNT {
            return None;
        }
        let mut speeds = self
            .challenges
            .iter()
            .map(|result| result.words_per_minute)
            .collect::<Vec<_>>();
        speeds.sort_unstable();
        Some(speeds[speeds.len() / 2])
    }

    /// The words per minute Time Saved should actually use: the measurement when
    /// there is one, the stand-in estimate otherwise, and `None` when there is
    /// neither — which is the hole the Usage pane shows.
    pub fn effective_wpm(&self) -> Option<u32> {
        self.measured_wpm().or(self.typed_estimate)
    }

    /// How many of the three challenges are done, for the "2 of 3" the challenge
    /// window shows.
    pub fn completed_challenges(&self) -> usize {
        self.challenges.len()
    }

    /// The passage the next challenge should use, or `None` when all three are
    /// finished. Passages are handed out in order, and a retry re-serves the
    /// slot that was abandoned rather than skipping ahead.
    pub fn next_passage_index(&self) -> Option<usize> {
        (0..TYPING_CHALLENGE_COUNT).find(|index| {
            !self
                .challenges
                .iter()
                .any(|result| result.passage_index == *index)
        })
    }
}

/// Score one Typing Challenge: correct whitespace words per minute.
///
/// Words are matched against the passage in order — the nth typed word against
/// the nth passage word — so a user cannot score by typing one easy word over
/// and over, and a single skipped word costs only the words that follow it in
/// that comparison rather than being silently forgiven.
///
/// Backspace is not modelled here at all. The window hands over the final text,
/// so corrections are free, which is how people actually type.
pub fn score_typing_challenge(passage: &str, typed: &str, seconds: u32) -> u32 {
    if seconds == 0 {
        return 0;
    }
    let expected = passage.split_whitespace();
    let actual = typed.split_whitespace();
    let correct = expected
        .zip(actual)
        .filter(|(expected, actual)| expected == actual)
        .count();

    (correct as f64 / f64::from(seconds) * 60.0).round() as u32
}

/// Store a finished Typing Challenge, replacing any earlier result for the same
/// passage so a retry overwrites its slot instead of adding a fourth.
pub fn record_typing_challenge(
    baseline: &mut TypingBaseline,
    passage_index: usize,
    words_per_minute: u32,
) {
    if passage_index >= TYPING_CHALLENGE_COUNT {
        return;
    }
    baseline
        .challenges
        .retain(|result| result.passage_index != passage_index);
    baseline.challenges.push(TypingChallengeResult {
        passage_index,
        words_per_minute,
    });
    baseline
        .challenges
        .sort_by_key(|result| result.passage_index);
}

/// Clear the three challenge results so the user can sit them again. Redo
/// replaces all three rather than topping up: a baseline mixing a run from last
/// year with two from today is not a measurement of anything.
pub fn redo_typing_challenges(baseline: &mut TypingBaseline) {
    baseline.challenges.clear();
}

/// Set or clear the typed estimate.
///
/// Refused once there is a measured baseline. The estimate exists to fill a
/// hole, and after three challenges there is no hole — letting a typed number
/// quietly override a measurement would make Time Saved worse, not better.
/// Out-of-range numbers are refused too rather than clamped, because clamping
/// 500 to 150 would silently answer a different question than the user asked.
pub fn apply_typed_estimate(
    baseline: &mut TypingBaseline,
    estimate: Option<u32>,
) -> Result<(), TypingBaselineError> {
    if baseline.measured_wpm().is_some() {
        return Err(TypingBaselineError::AlreadyMeasured);
    }
    if let Some(value) = estimate {
        if !TYPED_ESTIMATE_RANGE.contains(&value) {
            return Err(TypingBaselineError::EstimateOutOfRange(value));
        }
    }
    baseline.typed_estimate = estimate;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingBaselineError {
    /// The three Typing Challenges are done, so the typed estimate no longer
    /// applies and cannot be typed over the measurement.
    AlreadyMeasured,
    EstimateOutOfRange(u32),
}

impl std::fmt::Display for TypingBaselineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyMeasured => write!(
                f,
                "your typing speed has been measured, so the estimate no longer applies \
                 \u{2014} redo the typing challenges to change it"
            ),
            Self::EstimateOutOfRange(value) => write!(
                f,
                "a typing estimate must be between {} and {} words per minute, not {value}",
                TYPED_ESTIMATE_RANGE.start(),
                TYPED_ESTIMATE_RANGE.end()
            ),
        }
    }
}

impl std::error::Error for TypingBaselineError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(speeds: [u32; TYPING_CHALLENGE_COUNT]) -> TypingBaseline {
        let mut baseline = TypingBaseline::default();
        for (index, speed) in speeds.into_iter().enumerate() {
            record_typing_challenge(&mut baseline, index, speed);
        }
        baseline
    }

    #[test]
    fn three_shipped_passages_are_real_prose_and_long_enough_to_outrun() {
        // A fast typist at 150 WPM types 75 words in thirty seconds, so a passage
        // shorter than that would cap the score at the passage rather than the
        // typist.
        for passage in TYPING_CHALLENGE_PASSAGES {
            let words = passage.split_whitespace().count();
            assert!(words >= 75, "passage has only {words} words: {passage}");
        }
    }

    #[test]
    fn a_challenge_scores_correct_words_per_minute() {
        // Six correct words in thirty seconds is twelve a minute.
        let score = score_typing_challenge("the quick brown fox jumps over", "the quick brown fox jumps over", 30);
        assert_eq!(score, 12);
    }

    #[test]
    fn only_words_matching_the_passage_in_order_count() {
        // "brown" typed as "brwn" costs that word. Everything after it still
        // lines up, so the rest is still credited.
        let score = score_typing_challenge(
            "the quick brown fox jumps over",
            "the quick brwn fox jumps over",
            60,
        );
        assert_eq!(score, 5);

        // Typing one easy word repeatedly scores one word, not six.
        let score = score_typing_challenge("the quick brown fox jumps over", "the the the the the the", 60);
        assert_eq!(score, 1);
    }

    #[test]
    fn typing_less_than_the_passage_scores_only_what_was_typed() {
        let score = score_typing_challenge("the quick brown fox jumps over", "the quick brown", 60);
        assert_eq!(score, 3);

        assert_eq!(score_typing_challenge("the quick brown fox", "", 30), 0);
        assert_eq!(score_typing_challenge("the quick brown fox", "   ", 30), 0);
    }

    #[test]
    fn a_zero_second_challenge_scores_nothing_rather_than_dividing_by_zero() {
        assert_eq!(score_typing_challenge("the quick brown fox", "the quick brown fox", 0), 0);
    }

    #[test]
    fn the_measured_baseline_is_the_median_so_one_bad_run_does_not_decide_it() {
        // A phone call halfway through run two. The mean would say 47; the median
        // says 60, which is what this user types.
        assert_eq!(measured([60, 22, 62]).measured_wpm(), Some(60));
        assert_eq!(measured([40, 40, 40]).measured_wpm(), Some(40));
    }

    #[test]
    fn there_is_no_measured_baseline_until_all_three_challenges_are_done() {
        let mut baseline = TypingBaseline::default();
        assert_eq!(baseline.measured_wpm(), None);
        assert_eq!(baseline.next_passage_index(), Some(0));

        record_typing_challenge(&mut baseline, 0, 55);
        assert_eq!(baseline.measured_wpm(), None);
        assert_eq!(baseline.completed_challenges(), 1);
        assert_eq!(baseline.next_passage_index(), Some(1));

        record_typing_challenge(&mut baseline, 1, 58);
        assert_eq!(baseline.measured_wpm(), None);
        assert_eq!(baseline.next_passage_index(), Some(2));

        record_typing_challenge(&mut baseline, 2, 51);
        assert_eq!(baseline.measured_wpm(), Some(55));
        assert_eq!(baseline.next_passage_index(), None);
    }

    #[test]
    fn retrying_a_challenge_replaces_that_slot_rather_than_adding_a_fourth() {
        // Abort and retry: the aborted slot is served again, and the new score
        // takes its place.
        let mut baseline = measured([40, 41, 42]);

        record_typing_challenge(&mut baseline, 1, 70);

        assert_eq!(baseline.challenges.len(), TYPING_CHALLENGE_COUNT);
        assert_eq!(
            baseline.challenges[1],
            TypingChallengeResult {
                passage_index: 1,
                words_per_minute: 70
            }
        );
        assert_eq!(baseline.measured_wpm(), Some(42));
    }

    #[test]
    fn a_passage_index_past_the_three_challenges_is_ignored() {
        let mut baseline = TypingBaseline::default();
        record_typing_challenge(&mut baseline, TYPING_CHALLENGE_COUNT, 90);
        assert!(baseline.challenges.is_empty());
    }

    #[test]
    fn redo_clears_all_three_and_returns_to_the_estimate_if_there_is_one() {
        let mut baseline = measured([50, 52, 54]);
        baseline.typed_estimate = Some(45);
        assert_eq!(baseline.effective_wpm(), Some(52));

        redo_typing_challenges(&mut baseline);

        assert_eq!(baseline.measured_wpm(), None);
        assert_eq!(baseline.completed_challenges(), 0);
        assert_eq!(baseline.next_passage_index(), Some(0));
        assert_eq!(baseline.effective_wpm(), Some(45));
    }

    #[test]
    fn the_typed_estimate_is_a_stand_in_the_measurement_replaces() {
        let mut baseline = TypingBaseline::default();
        apply_typed_estimate(&mut baseline, Some(45)).unwrap();
        assert_eq!(baseline.effective_wpm(), Some(45));

        for (index, speed) in [60, 62, 64].into_iter().enumerate() {
            record_typing_challenge(&mut baseline, index, speed);
        }

        assert_eq!(baseline.effective_wpm(), Some(62));
    }

    #[test]
    fn a_typed_estimate_cannot_be_typed_over_a_measured_baseline() {
        let mut baseline = measured([60, 62, 64]);

        let error = apply_typed_estimate(&mut baseline, Some(140)).unwrap_err();

        assert_eq!(error, TypingBaselineError::AlreadyMeasured);
        assert_eq!(baseline.effective_wpm(), Some(62));
    }

    #[test]
    fn a_typing_estimate_outside_ten_to_one_fifty_is_refused_not_clamped() {
        let mut baseline = TypingBaseline::default();

        assert_eq!(
            apply_typed_estimate(&mut baseline, Some(9)),
            Err(TypingBaselineError::EstimateOutOfRange(9))
        );
        assert_eq!(
            apply_typed_estimate(&mut baseline, Some(151)),
            Err(TypingBaselineError::EstimateOutOfRange(151))
        );
        assert_eq!(baseline.typed_estimate, None);

        apply_typed_estimate(&mut baseline, Some(10)).unwrap();
        assert_eq!(baseline.typed_estimate, Some(10));
        apply_typed_estimate(&mut baseline, Some(150)).unwrap();
        assert_eq!(baseline.typed_estimate, Some(150));
    }

    #[test]
    fn clearing_the_estimate_returns_time_saved_to_a_hole() {
        let mut baseline = TypingBaseline::default();
        apply_typed_estimate(&mut baseline, Some(45)).unwrap();

        apply_typed_estimate(&mut baseline, None).unwrap();

        assert_eq!(baseline.typed_estimate, None);
        assert_eq!(baseline.effective_wpm(), None);
    }

    #[test]
    fn a_fresh_baseline_has_neither_a_measurement_nor_an_estimate() {
        let baseline = TypingBaseline::default();
        assert_eq!(baseline.effective_wpm(), None);
        assert_eq!(baseline.completed_challenges(), 0);
        assert!(baseline.typed_estimate.is_none());
    }
}
